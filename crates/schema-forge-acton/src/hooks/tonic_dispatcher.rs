//! Real gRPC [`HookDispatcher`] implementation built on tonic and
//! `prost-reflect`.
//!
//! At construction time, the dispatcher scans every binding in
//! [`HooksConfig`], loads its `descriptor_path` `FileDescriptorSet` into
//! a [`DescriptorPool`], and resolves the per-event service + method
//! descriptors. At call time it builds a [`DynamicMessage`] from the
//! [`HookInvocation`] payload, sends it over a pooled tonic
//! [`Channel`], and decodes the response back into a [`HookOutcome`].
//!
//! The wire convention is:
//!
//! * **Service name**: `{Schema}Hooks` (case-insensitive simple-name match
//!   inside the pool — package may be anything).
//! * **Method name**: PascalCase form of the lifecycle event
//!   (`BeforeChange`, `AfterChange`, …).
//! * **Request fields**: schema fields by name, plus optional `operation`,
//!   `user_id`, `entity_id` system fields if declared in the proto.
//! * **Response fields** (`before_*` only): optional `abort_reason`
//!   string; any other set field is treated as a modified entity field.
//!
//! Failure to load a descriptor or to resolve a binding's service/method
//! is **fatal** at construction time — operators get a clear error rather
//! than silent runtime drift.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Buf, BufMut};
use http::uri::PathAndQuery;
use prost_reflect::prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, Kind, MapKey, MessageDescriptor, ReflectMessage, Value,
};
use schema_forge_core::types::{DynamicValue, HookEvent};
use tokio::sync::Mutex;
use tonic::client::Grpc;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Status};
use tracing::{debug, warn};

use super::credential::HookCredentialSource;
use super::{
    HookBinding, HookDispatcher, HookError, HookInvocation, HookOutcome, HooksConfig,
    DEFAULT_HOOK_TIMEOUT_MS,
};

/// gRPC metadata key carrying the bearer credential.
///
/// acton-service's `GrpcTokenAuthLayer` reads the HTTP `authorization` header,
/// which is where tonic puts this metadata entry.
const AUTHORIZATION_METADATA: &str = "authorization";

/// Configuration knobs that influence dispatcher construction (timeouts
/// for the channel, descriptor loader, etc.). Distinct from
/// [`HooksConfig`] which describes per-binding policy.
#[derive(Debug, Clone)]
pub struct TonicDispatcherConfig {
    /// Connect timeout applied when opening a tonic [`Channel`] to a hook
    /// endpoint. Defaults to 2 seconds.
    pub connect_timeout: Duration,

    /// Supplies the bearer credential presented on every hook call.
    ///
    /// `None` sends hook calls unauthenticated, which a hook service built
    /// from the scaffold will reject. It exists for tests and for embedders
    /// whose hook services authenticate the forge some other way; the `serve`
    /// command always supplies one.
    pub credential: Option<Arc<dyn HookCredentialSource>>,
}

impl TonicDispatcherConfig {
    /// Default connect timeout: 2 seconds.
    const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
}

// Hand-written rather than derived so `connect_timeout` gets its documented
// default instead of `Duration::ZERO`, which would fail every connection.
impl Default for TonicDispatcherConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Self::DEFAULT_CONNECT_TIMEOUT,
            credential: None,
        }
    }
}

/// Whether an endpoint may be dialed under the configured plaintext policy.
///
/// A hook call carries the entity's field snapshot, the triggering user's
/// subject, and the bearer credential the forge presents. All three are exposed
/// on a cleartext hop, and the credential is replayable from it, so plaintext
/// is refused unless an operator has explicitly accepted that.
///
/// Pure, so the policy is testable without opening a socket — and it runs
/// *before* the connection is opened, so a refusal means nothing left the
/// process.
fn endpoint_is_permitted(endpoint: &str, allow_plaintext: bool) -> bool {
    allow_plaintext || endpoint.starts_with("https://")
}

/// Resolved per-binding state cached at construction time.
#[derive(Debug)]
struct ResolvedBinding {
    request_descriptor: MessageDescriptor,
    response_descriptor: MessageDescriptor,
    path: PathAndQuery,
}

/// Real tonic + `prost-reflect` dispatcher.
#[derive(Debug)]
pub struct TonicHookDispatcher {
    config: TonicDispatcherConfig,
    /// (schema, event) -> resolved descriptor
    bindings: HashMap<(String, HookEvent), ResolvedBinding>,
    /// endpoint URL -> tonic Channel (lazily connected, then cached).
    channels: Mutex<HashMap<String, Channel>>,
    /// TLS settings applied to every `https://` endpoint. Resolved once at
    /// construction so a bad certificate or key fails at startup rather than
    /// on the first entity write that happens to fire a hook.
    tls: ClientTlsConfig,
    /// Whether `http://` endpoints may be dialed. See
    /// [`HooksConfig::allow_plaintext`].
    allow_plaintext: bool,
}

impl TonicHookDispatcher {
    /// Build a dispatcher from the given hooks configuration. Loads every
    /// `descriptor_path` exactly once and resolves the per-binding service
    /// + method descriptors.
    ///
    /// Returns an error if any descriptor file is unreadable, malformed,
    /// or does not contain a service/method matching a binding.
    pub fn new(hooks: &HooksConfig, dispatcher: TonicDispatcherConfig) -> Result<Self, HookError> {
        let mut pools_by_path: HashMap<String, DescriptorPool> = HashMap::new();
        let mut bindings: HashMap<(String, HookEvent), ResolvedBinding> = HashMap::new();

        for binding in &hooks.bindings {
            // Checked here as well as at dispatch. The per-call check is what
            // guarantees nothing is sent in the clear; this one is what makes
            // the operator find out at boot instead of on the first hooked
            // write — and it is not subject to `required = false`, which would
            // otherwise downgrade a misconfigured endpoint to a warning and
            // let the hook silently never run.
            if !endpoint_is_permitted(&binding.endpoint, hooks.allow_plaintext) {
                return Err(HookError::InsecureEndpoint {
                    endpoint: binding.endpoint.clone(),
                });
            }

            let path = binding
                .descriptor_path
                .as_deref()
                .ok_or_else(|| HookError::Internal {
                    message: format!(
                        "binding {schema}/{event:?} has no descriptor_path",
                        schema = binding.schema,
                        event = binding.event
                    ),
                })?;

            let pool = if let Some(p) = pools_by_path.get(path) {
                p.clone()
            } else {
                let bytes = std::fs::read(path).map_err(|e| HookError::Internal {
                    message: format!("failed to read descriptor {path}: {e}"),
                })?;
                let pool =
                    DescriptorPool::decode(bytes.as_slice()).map_err(|e| HookError::Internal {
                        message: format!("failed to decode descriptor {path}: {e}"),
                    })?;
                pools_by_path.insert(path.to_string(), pool.clone());
                pool
            };

            let resolved = resolve_binding(&pool, binding)?;
            bindings.insert((binding.schema.clone(), binding.event), resolved);
        }

        // Resolving the client identity here, rather than at first connect,
        // means an unreadable certificate or a key that does not match its
        // certificate is a startup failure. Deferred, it would surface as an
        // intermittent hook outage on whichever write first triggered a hook.
        let tls = match &hooks.client_identity {
            Some(identity) if identity.enabled => {
                acton_service::client_tls::tonic_client_tls_config(identity).map_err(|e| {
                    HookError::Internal {
                        message: format!("invalid hook client identity: {e}"),
                    }
                })?
            }
            // No client certificate: verify the peer against the built-in web
            // PKI roots and present nothing. Appropriate when the hook service
            // authenticates the forge by bearer token alone.
            _ => ClientTlsConfig::new().with_enabled_roots(),
        };

        if dispatcher.credential.is_none() && !hooks.bindings.is_empty() {
            warn!(
                "hook dispatch configured with no credential source: calls will carry no \
                 `authorization` metadata and any hook service with `[token]` configured will \
                 reject them"
            );
        }

        Ok(Self {
            config: dispatcher,
            bindings,
            channels: Mutex::new(HashMap::new()),
            tls,
            allow_plaintext: hooks.allow_plaintext,
        })
    }

    /// Total number of resolved bindings — useful for diagnostics.
    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }

    async fn channel_for(&self, endpoint: &str) -> Result<Channel, HookError> {
        if let Some(c) = self.channels.lock().await.get(endpoint) {
            return Ok(c.clone());
        }
        // Checked before the cache is populated and before any socket is
        // opened, so a refused endpoint never sends a byte.
        if !endpoint_is_permitted(endpoint, self.allow_plaintext) {
            return Err(HookError::InsecureEndpoint {
                endpoint: endpoint.to_string(),
            });
        }
        let ep = Endpoint::from_str(endpoint).map_err(|e| HookError::Internal {
            message: format!("invalid endpoint {endpoint}: {e}"),
        })?;
        let ep = ep.connect_timeout(self.config.connect_timeout);
        // `tls_config` on an `http://` endpoint is an error in tonic, so it is
        // applied only where it applies. Under `allow_plaintext` the operator
        // has already accepted that this hop is unprotected.
        let ep = if endpoint.starts_with("https://") {
            ep.tls_config(self.tls.clone())
                .map_err(|e| HookError::Internal {
                    message: format!("failed to apply TLS config for {endpoint}: {e}"),
                })?
        } else {
            ep
        };
        let channel = ep.connect().await.map_err(|e| HookError::Unavailable {
            endpoint: endpoint.to_string(),
            message: e.to_string(),
        })?;
        self.channels
            .lock()
            .await
            .insert(endpoint.to_string(), channel.clone());
        Ok(channel)
    }

    async fn invoke(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
        config_timeout_ms: u32,
    ) -> Result<DynamicMessage, HookError> {
        let resolved = self
            .bindings
            .get(&(binding.schema.clone(), binding.event))
            .ok_or_else(|| HookError::Internal {
                message: format!(
                    "no resolved descriptor for {}/{:?}",
                    binding.schema, binding.event
                ),
            })?;

        let request_msg = build_request(&resolved.request_descriptor, &invocation)?;
        let codec = DynamicCodec::new(resolved.response_descriptor.clone());

        let channel = self.channel_for(&binding.endpoint).await?;
        let mut grpc = Grpc::new(channel);
        grpc.ready().await.map_err(|e| HookError::Unavailable {
            endpoint: binding.endpoint.clone(),
            message: e.to_string(),
        })?;

        let mut request = Request::new(request_msg);
        let timeout = Duration::from_millis(config_timeout_ms as u64);
        request.set_timeout(timeout);

        // Minted per call rather than cached, so a short-lived credential is
        // never presented after it has expired.
        if let Some(ref source) = self.config.credential {
            let value = format!("Bearer {}", source.bearer()?);
            let value = value.parse().map_err(|e| HookError::Internal {
                message: format!("minted hook credential is not valid metadata: {e}"),
            })?;
            request.metadata_mut().insert(AUTHORIZATION_METADATA, value);
        }

        let call = grpc.unary(request, resolved.path.clone(), codec);
        let response = match tokio::time::timeout(timeout, call).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(status)) => {
                if status.code() == tonic::Code::DeadlineExceeded {
                    return Err(HookError::Timeout {
                        endpoint: binding.endpoint.clone(),
                        timeout_ms: config_timeout_ms,
                    });
                }
                return Err(HookError::Unavailable {
                    endpoint: binding.endpoint.clone(),
                    message: status.to_string(),
                });
            }
            Err(_) => {
                return Err(HookError::Timeout {
                    endpoint: binding.endpoint.clone(),
                    timeout_ms: config_timeout_ms,
                });
            }
        };

        Ok(response.into_inner())
    }
}

#[async_trait]
impl HookDispatcher for TonicHookDispatcher {
    async fn call_before(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<HookOutcome, HookError> {
        debug!(
            schema = %binding.schema,
            event = ?binding.event,
            endpoint = %binding.endpoint,
            "tonic dispatch (before)"
        );
        let timeout = binding.timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS);
        let response = self.invoke(binding, invocation, timeout).await?;
        Ok(decode_outcome(&response))
    }

    async fn call_after(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<(), HookError> {
        debug!(
            schema = %binding.schema,
            event = ?binding.event,
            endpoint = %binding.endpoint,
            "tonic dispatch (after)"
        );
        let timeout = binding.timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS);
        let _ = self.invoke(binding, invocation, timeout).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Binding resolution
// ---------------------------------------------------------------------------

fn resolve_binding(
    pool: &DescriptorPool,
    binding: &HookBinding,
) -> Result<ResolvedBinding, HookError> {
    let want_service = format!("{}Hooks", binding.schema);
    let want_method = event_to_method(binding.event);

    let service = pool
        .services()
        .find(|s| s.name() == want_service)
        .ok_or_else(|| HookError::Internal {
            message: format!(
                "descriptor for binding {}/{:?} has no service `{}`",
                binding.schema, binding.event, want_service
            ),
        })?;

    let method = service
        .methods()
        .find(|m| m.name() == want_method)
        .ok_or_else(|| HookError::Internal {
            message: format!(
                "service `{}` has no method `{}`",
                service.full_name(),
                want_method
            ),
        })?;

    let path_str = format!("/{}/{}", service.full_name(), method.name());
    let path =
        PathAndQuery::from_maybe_shared(path_str.clone()).map_err(|e| HookError::Internal {
            message: format!("invalid grpc path `{path_str}`: {e}"),
        })?;

    Ok(ResolvedBinding {
        request_descriptor: method.input(),
        response_descriptor: method.output(),
        path,
    })
}

fn event_to_method(event: HookEvent) -> &'static str {
    match event {
        HookEvent::BeforeValidate => "BeforeValidate",
        HookEvent::BeforeChange => "BeforeChange",
        HookEvent::AfterChange => "AfterChange",
        HookEvent::BeforeRead => "BeforeRead",
        HookEvent::AfterRead => "AfterRead",
        HookEvent::BeforeDelete => "BeforeDelete",
        HookEvent::AfterDelete => "AfterDelete",
        HookEvent::BeforeUpload => "BeforeUpload",
        HookEvent::AfterUpload => "AfterUpload",
        HookEvent::OnScanComplete => "OnScanComplete",
    }
}

// ---------------------------------------------------------------------------
// Request encoding (DynamicValue -> DynamicMessage)
// ---------------------------------------------------------------------------

fn build_request(
    descriptor: &MessageDescriptor,
    invocation: &HookInvocation,
) -> Result<DynamicMessage, HookError> {
    let mut msg = DynamicMessage::new(descriptor.clone());

    for field in descriptor.fields() {
        let name = field.name();
        if invocation.event == HookEvent::AfterChange
            && field.number() == 4
            && field.is_list()
            && matches!(field.kind(), Kind::String)
        {
            checked_set(
                &mut msg,
                name,
                Value::List(
                    invocation
                        .changed_fields
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            )?;
            continue;
        }
        if invocation.event == HookEvent::AfterChange && is_previous_metadata(&field) {
            let values = invocation
                .previous
                .iter()
                .map(|(name, value)| {
                    let json = crate::conversions::dynamic_value_to_json(value);
                    json_to_proto_value(descriptor.parent_pool(), &json)
                        .map(|value| (MapKey::String(name.clone()), value))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            checked_set(&mut msg, name, Value::Map(values))?;
            continue;
        }
        match name {
            "operation" => {
                checked_set(&mut msg, name, Value::String(invocation.operation.clone()))?;
            }
            "user_id" => {
                if let Some(uid) = &invocation.user_id {
                    checked_set(&mut msg, name, Value::String(uid.clone()))?;
                }
            }
            "entity_id" => {
                if let Some(eid) = &invocation.entity_id {
                    checked_set(&mut msg, name, Value::String(eid.clone()))?;
                }
            }
            _ => {
                if let Some(dv) = invocation.fields.get(name) {
                    if let Some(v) = dynamic_value_to_proto(dv, &field.kind(), field.is_list()) {
                        checked_set(&mut msg, name, v)?;
                    }
                }
            }
        }
    }

    Ok(msg)
}

fn is_previous_metadata(field: &prost_reflect::FieldDescriptor) -> bool {
    if field.number() != 5 || !field.is_map() {
        return false;
    }
    let Kind::Message(entry) = field.kind() else {
        return false;
    };
    entry.get_field(1).is_some_and(|key| matches!(key.kind(), Kind::String))
        && entry.get_field(2).is_some_and(|value| {
            matches!(value.kind(), Kind::Message(message) if message.full_name() == "google.protobuf.Value")
        })
}

/// Encode an entity JSON value in the protobuf well-known Value shape.
/// Protobuf numbers are doubles; large integers use decimal strings so the
/// previous snapshot never silently rounds an identifier or counter.
fn json_to_proto_value(
    pool: &DescriptorPool,
    json: &serde_json::Value,
) -> Result<Value, HookError> {
    let mut message = well_known_message(pool, "Value")?;
    let (name, value) = match json {
        serde_json::Value::Null => ("null_value", Value::EnumNumber(0)),
        serde_json::Value::Bool(value) => ("bool_value", Value::Bool(*value)),
        serde_json::Value::String(value) => ("string_value", Value::String(value.clone())),
        serde_json::Value::Number(value) => {
            const MAX_EXACT_INTEGER: u64 = 1 << 53;
            if value
                .as_i64()
                .is_some_and(|value| value.unsigned_abs() > MAX_EXACT_INTEGER)
                || value
                    .as_u64()
                    .is_some_and(|value| value > MAX_EXACT_INTEGER)
            {
                ("string_value", Value::String(value.to_string()))
            } else {
                let number = value.as_f64().ok_or_else(|| HookError::Protocol {
                    message: format!(
                        "previous value {value} cannot be encoded as a protobuf number"
                    ),
                })?;
                ("number_value", Value::F64(number))
            }
        }
        serde_json::Value::Array(values) => {
            let mut list = well_known_message(pool, "ListValue")?;
            let values = values
                .iter()
                .map(|value| json_to_proto_value(pool, value))
                .collect::<Result<Vec<_>, _>>()?;
            checked_set(&mut list, "values", Value::List(values))?;
            ("list_value", Value::Message(list))
        }
        serde_json::Value::Object(values) => {
            let mut object = well_known_message(pool, "Struct")?;
            let values = values
                .iter()
                .map(|(name, value)| {
                    json_to_proto_value(pool, value)
                        .map(|value| (MapKey::String(name.clone()), value))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            checked_set(&mut object, "fields", Value::Map(values))?;
            ("struct_value", Value::Message(object))
        }
    };
    checked_set(&mut message, name, value)?;
    Ok(Value::Message(message))
}

fn well_known_message(pool: &DescriptorPool, name: &str) -> Result<DynamicMessage, HookError> {
    pool.get_message_by_name(&format!("google.protobuf.{name}"))
        .map(DynamicMessage::new)
        .ok_or_else(|| HookError::Protocol {
            message: format!(
                "hook descriptor is missing google.protobuf.{name} for previous values"
            ),
        })
}

/// Wrapper around [`DynamicMessage::try_set_field_by_name`] that translates a
/// `prost-reflect` rejection into [`HookError::Protocol`] instead of a panic.
/// This is the firewall between user-controlled payloads and the panic-based
/// `set_field_by_name` API: any cardinality / type mismatch becomes a clean
/// 4xx-class error, never a 500 from `tower_http::catch_panic`.
fn checked_set(msg: &mut DynamicMessage, name: &str, value: Value) -> Result<(), HookError> {
    msg.try_set_field_by_name(name, value)
        .map_err(|e| HookError::Protocol {
            message: format!("hook field `{name}` does not match proto descriptor: {e}"),
        })
}

/// Convert a [`DynamicValue`] into a `prost-reflect` [`Value`] suitable for
/// the target proto field.
///
/// `is_list` reflects whether the destination proto field is `repeated`.
/// When `true`, scalar inputs are wrapped into a single-element list and
/// list inputs are passed through; when `false`, list inputs against a
/// scalar field are rejected (returning `None` so the caller can decide
/// whether to drop or error — `build_request` errors via the strict
/// [`checked_set`] path).
fn dynamic_value_to_proto(value: &DynamicValue, kind: &Kind, is_list: bool) -> Option<Value> {
    if is_list {
        return list_value_for_field(value, kind);
    }
    scalar_value_for_field(value, kind)
}

fn scalar_value_for_field(value: &DynamicValue, kind: &Kind) -> Option<Value> {
    match value {
        DynamicValue::Null => None,
        DynamicValue::Text(s) => Some(match kind {
            Kind::String => Value::String(s.clone()),
            Kind::Bytes => Value::Bytes(s.clone().into_bytes().into()),
            _ => Value::String(s.clone()),
        }),
        DynamicValue::Integer(i) => Some(match kind {
            Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => Value::I32(*i as i32),
            Kind::Uint32 | Kind::Fixed32 => Value::U32(*i as u32),
            Kind::Uint64 | Kind::Fixed64 => Value::U64(*i as u64),
            Kind::Float => Value::F32(*i as f32),
            Kind::Double => Value::F64(*i as f64),
            _ => Value::I64(*i),
        }),
        DynamicValue::Float(f) => Some(match kind {
            Kind::Float => Value::F32(*f as f32),
            _ => Value::F64(*f),
        }),
        DynamicValue::Boolean(b) => Some(Value::Bool(*b)),
        DynamicValue::DateTime(dt) => Some(Value::String(dt.to_rfc3339())),
        DynamicValue::Enum(s) => Some(Value::String(s.clone())),
        DynamicValue::Json(j) => Some(Value::String(j.to_string())),
        DynamicValue::Ref(id) => Some(Value::String(id.to_string())),
        // List-on-scalar: no value to set; `try_set_field_by_name` will not
        // be called and the field stays unset.
        DynamicValue::Array(_) | DynamicValue::RefArray(_) => None,
        DynamicValue::Composite(_) => None,
        _ => None,
    }
}

/// Build a [`Value::List`] for a `repeated` proto field. Scalar inputs are
/// promoted into a single-element list so a hook author can send either
/// `"a"` or `["a"]` and have it work — matching the JSON ingest behavior.
fn list_value_for_field(value: &DynamicValue, kind: &Kind) -> Option<Value> {
    match value {
        DynamicValue::Null => None,
        DynamicValue::Array(items) => Some(Value::List(
            items
                .iter()
                .filter_map(|v| scalar_value_for_field(v, kind))
                .collect(),
        )),
        DynamicValue::RefArray(ids) => Some(Value::List(
            ids.iter().map(|i| Value::String(i.to_string())).collect(),
        )),
        // Scalar-on-repeated: promote into a one-element list.
        scalar => scalar_value_for_field(scalar, kind).map(|v| Value::List(vec![v])),
    }
}

// ---------------------------------------------------------------------------
// Response decoding (DynamicMessage -> HookOutcome)
// ---------------------------------------------------------------------------

fn decode_outcome(msg: &DynamicMessage) -> HookOutcome {
    let mut outcome = HookOutcome::default();
    let descriptor = msg.descriptor();

    for field in descriptor.fields() {
        let name = field.name();
        if !msg.has_field_by_name(name) {
            continue;
        }
        let value = match msg.get_field_by_name(name) {
            Some(v) => v.into_owned(),
            None => continue,
        };

        if name == "abort_reason" {
            if let Some(s) = value.as_str() {
                if !s.is_empty() {
                    outcome.abort_reason = Some(s.to_string());
                }
            }
            continue;
        }

        if let Some(dv) = proto_value_to_dynamic(&value) {
            outcome
                .modified_fields
                .get_or_insert_with(Default::default)
                .insert(name.to_string(), dv);
        }
    }

    outcome
}

fn proto_value_to_dynamic(value: &Value) -> Option<DynamicValue> {
    match value {
        Value::Bool(b) => Some(DynamicValue::Boolean(*b)),
        Value::I32(i) => Some(DynamicValue::Integer(*i as i64)),
        Value::I64(i) => Some(DynamicValue::Integer(*i)),
        Value::U32(u) => Some(DynamicValue::Integer(*u as i64)),
        Value::U64(u) => Some(DynamicValue::Integer(*u as i64)),
        Value::F32(f) => Some(DynamicValue::Float(*f as f64)),
        Value::F64(f) => Some(DynamicValue::Float(*f)),
        Value::String(s) => Some(DynamicValue::Text(s.clone())),
        Value::Bytes(b) => Some(DynamicValue::Text(String::from_utf8_lossy(b).into_owned())),
        Value::EnumNumber(n) => Some(DynamicValue::Integer(*n as i64)),
        Value::List(items) => Some(DynamicValue::Array(
            items.iter().filter_map(proto_value_to_dynamic).collect(),
        )),
        Value::Message(_) | Value::Map(_) => {
            warn!("nested message/map fields in hook response are not yet supported");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// DynamicCodec — tonic Codec backed by prost-reflect
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DynamicCodec {
    response_descriptor: MessageDescriptor,
}

impl DynamicCodec {
    fn new(response_descriptor: MessageDescriptor) -> Self {
        Self {
            response_descriptor,
        }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            response_descriptor: self.response_descriptor.clone(),
        }
    }
}

#[derive(Debug)]
struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        let bytes = item.encode_to_vec();
        buf.put_slice(&bytes);
        Ok(())
    }
}

#[derive(Debug)]
struct DynamicDecoder {
    response_descriptor: MessageDescriptor,
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let len = buf.remaining();
        let mut bytes = vec![0u8; len];
        buf.copy_to_slice(&mut bytes);
        let msg = DynamicMessage::decode(self.response_descriptor.clone(), bytes.as_slice())
            .map_err(|e| Status::internal(format!("decode failure: {e}")))?;
        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_descriptor_set() -> prost_types::FileDescriptorSet {
        prost_types::FileDescriptorSet::decode(
            include_bytes!(env!("TRANSLATION_HOOKS_DESCRIPTOR")).as_slice(),
        )
        .unwrap()
    }

    #[test]
    fn old_descriptors_keep_business_fields_named_like_change_metadata() {
        let mut set = test_descriptor_set();
        let request = set
            .file
            .iter_mut()
            .flat_map(|file| &mut file.message_type)
            .find(|message| message.name.as_deref() == Some("TranslationAfterChangeRequest"))
            .unwrap();
        request
            .field
            .retain(|field| !matches!(field.number, Some(4 | 5)));
        for (name, number, label) in [
            (
                "changed_fields",
                103,
                prost_types::field_descriptor_proto::Label::Repeated,
            ),
            (
                "previous",
                104,
                prost_types::field_descriptor_proto::Label::Optional,
            ),
        ] {
            request.field.push(prost_types::FieldDescriptorProto {
                name: Some(name.into()),
                number: Some(number),
                label: Some(label.into()),
                r#type: Some(prost_types::field_descriptor_proto::Type::String.into()),
                ..Default::default()
            });
        }
        let pool = DescriptorPool::from_file_descriptor_set(set).unwrap();
        let descriptor = pool
            .get_message_by_name("schema_forge_test.TranslationAfterChangeRequest")
            .unwrap();
        let invocation = HookInvocation {
            schema: "Translation".into(),
            event: HookEvent::AfterChange,
            operation: "update".into(),
            user_id: None,
            entity_id: None,
            fields: std::collections::BTreeMap::from([
                (
                    "changed_fields".into(),
                    DynamicValue::Array(vec![DynamicValue::Text("business".into())]),
                ),
                (
                    "previous".into(),
                    DynamicValue::Text("business previous".into()),
                ),
            ]),
            changed_fields: vec!["actual change".into()],
            previous: Default::default(),
        };
        let request = build_request(&descriptor, &invocation).unwrap();
        assert_eq!(
            request
                .get_field_by_name("changed_fields")
                .unwrap()
                .as_ref(),
            &Value::List(vec![Value::String("business".into())])
        );
        assert_eq!(
            request.get_field_by_name("previous").unwrap().as_ref(),
            &Value::String("business previous".into())
        );
    }

    #[test]
    fn previous_numbers_preserve_large_integer_precision() {
        let pool = DescriptorPool::from_file_descriptor_set(test_descriptor_set()).unwrap();
        for (json, expected_field, expected) in [
            (
                serde_json::json!(1_u64 << 53),
                "number_value",
                Value::F64((1_u64 << 53) as f64),
            ),
            (
                serde_json::json!((1_u64 << 53) + 1),
                "string_value",
                Value::String("9007199254740993".into()),
            ),
            (
                serde_json::json!(i64::MIN),
                "string_value",
                Value::String(i64::MIN.to_string()),
            ),
            (
                serde_json::json!(u64::MAX),
                "string_value",
                Value::String(u64::MAX.to_string()),
            ),
        ] {
            let Value::Message(value) = json_to_proto_value(&pool, &json).unwrap() else {
                panic!("expected protobuf Value message");
            };
            assert_eq!(
                value.get_field_by_name(expected_field).unwrap().as_ref(),
                &expected
            );
        }
    }

    #[test]
    fn renamed_metadata_fields_are_encoded_by_tag_and_type() {
        let mut set = test_descriptor_set();
        let request = set
            .file
            .iter_mut()
            .flat_map(|file| &mut file.message_type)
            .find(|message| message.name.as_deref() == Some("TranslationAfterChangeRequest"))
            .unwrap();
        for field in &mut request.field {
            match field.number {
                Some(4) => field.name = Some("schemaforge_changed_fields_2".into()),
                Some(5) => field.name = Some("schemaforge_previous_2".into()),
                _ => {}
            }
        }
        let pool = DescriptorPool::from_file_descriptor_set(set).unwrap();
        let descriptor = pool
            .get_message_by_name("schema_forge_test.TranslationAfterChangeRequest")
            .unwrap();
        let invocation = HookInvocation {
            schema: "Translation".into(),
            event: HookEvent::AfterChange,
            operation: "patch".into(),
            user_id: None,
            entity_id: None,
            fields: Default::default(),
            changed_fields: vec!["source_text".into()],
            previous: std::collections::BTreeMap::from([(
                "source_text".into(),
                DynamicValue::Null,
            )]),
        };
        let request = build_request(&descriptor, &invocation).unwrap();
        assert_eq!(
            request
                .get_field_by_name("schemaforge_changed_fields_2")
                .unwrap()
                .as_ref(),
            &Value::List(vec![Value::String("source_text".into())])
        );
        let value = request.get_field_by_name("schemaforge_previous_2").unwrap();
        let Value::Map(previous) = value.as_ref() else {
            panic!("expected previous map");
        };
        let Value::Message(previous) = &previous[&MapKey::String("source_text".into())] else {
            panic!("expected protobuf Value");
        };
        assert!(previous.has_field_by_name("null_value"));
    }

    #[test]
    fn event_method_names() {
        assert_eq!(event_to_method(HookEvent::BeforeChange), "BeforeChange");
        assert_eq!(event_to_method(HookEvent::AfterDelete), "AfterDelete");
    }

    #[test]
    fn dynamic_value_text_to_string_kind() {
        let v = dynamic_value_to_proto(&DynamicValue::Text("hi".into()), &Kind::String, false);
        assert!(matches!(v, Some(Value::String(s)) if s == "hi"));
    }

    #[test]
    fn dynamic_value_integer_to_int64() {
        let v = dynamic_value_to_proto(&DynamicValue::Integer(42), &Kind::Int64, false);
        assert!(matches!(v, Some(Value::I64(42))));
    }

    #[test]
    fn dynamic_value_null_skipped() {
        assert!(dynamic_value_to_proto(&DynamicValue::Null, &Kind::String, false).is_none());
    }

    #[test]
    fn dynamic_value_array_routes_to_list() {
        let v = dynamic_value_to_proto(
            &DynamicValue::Array(vec![
                DynamicValue::Text("a".into()),
                DynamicValue::Text("b".into()),
            ]),
            &Kind::String,
            true,
        );
        match v {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(&items[0], Value::String(s) if s == "a"));
                assert!(matches!(&items[1], Value::String(s) if s == "b"));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn dynamic_value_scalar_promoted_to_singleton_list_for_repeated_field() {
        let v = dynamic_value_to_proto(&DynamicValue::Text("solo".into()), &Kind::String, true);
        match v {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], Value::String(s) if s == "solo"));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn dynamic_value_list_against_scalar_field_yields_none() {
        let v = dynamic_value_to_proto(
            &DynamicValue::Array(vec![DynamicValue::Text("a".into())]),
            &Kind::String,
            false,
        );
        assert!(
            v.is_none(),
            "list against scalar field must not produce a value"
        );
    }

    #[test]
    fn proto_value_string_to_text() {
        let dv = proto_value_to_dynamic(&Value::String("ok".into()));
        assert!(matches!(dv, Some(DynamicValue::Text(s)) if s == "ok"));
    }

    #[test]
    fn proto_value_bool_to_boolean() {
        assert!(matches!(
            proto_value_to_dynamic(&Value::Bool(true)),
            Some(DynamicValue::Boolean(true))
        ));
    }

    /// Building a dispatcher with no bindings should succeed.
    #[test]
    fn empty_bindings_construct_ok() {
        let cfg = HooksConfig::default();
        let d = TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).unwrap();
        assert_eq!(d.binding_count(), 0);
    }

    /// A binding without descriptor_path is rejected.
    #[test]
    fn binding_without_descriptor_path_errors() {
        let cfg = HooksConfig {
            enabled: true,
            bindings: vec![HookBinding {
                schema: "X".into(),
                event: HookEvent::BeforeChange,
                endpoint: "https://x".into(),
                timeout_ms: None,
                required: false,
                descriptor_path: None,
            }],
            ..HooksConfig::default()
        };
        let err = TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).unwrap_err();
        assert!(matches!(err, HookError::Internal { .. }));
    }

    /// A plaintext binding must fail the boot, not the first hooked write.
    /// Under `required = false` a dispatch-time refusal would be logged and
    /// swallowed, leaving a hook that silently never runs.
    #[test]
    fn plaintext_binding_refused_at_construction() {
        let cfg = HooksConfig {
            enabled: true,
            bindings: vec![HookBinding {
                schema: "X".into(),
                event: HookEvent::BeforeChange,
                endpoint: "http://hook:9090".into(),
                timeout_ms: None,
                required: false,
                descriptor_path: Some("/nonexistent".into()),
            }],
            ..HooksConfig::default()
        };
        let err = TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).unwrap_err();
        assert!(
            matches!(err, HookError::InsecureEndpoint { .. }),
            "expected InsecureEndpoint, got {err:?}"
        );
    }

    #[test]
    fn plaintext_endpoint_refused_by_default() {
        assert!(!endpoint_is_permitted("http://hook:9090", false));
    }

    #[test]
    fn tls_endpoint_permitted_without_opt_in() {
        assert!(endpoint_is_permitted("https://hook:9090", false));
    }

    #[test]
    fn plaintext_endpoint_permitted_once_opted_in() {
        assert!(endpoint_is_permitted("http://127.0.0.1:9090", true));
    }

    /// An `https://` prefix must be matched as a scheme, not found anywhere in
    /// the string — `http://evil/?next=https://ok` would otherwise pass.
    #[test]
    fn tls_scheme_is_matched_at_the_start_only() {
        assert!(!endpoint_is_permitted(
            "http://evil.example/?next=https://hook",
            false
        ));
    }

    /// The refusal has to happen before a socket is opened. This endpoint
    /// resolves to nothing, so a `Unavailable` verdict would prove the
    /// dispatcher tried to connect before checking.
    #[tokio::test]
    async fn dispatch_to_plaintext_endpoint_fails_without_connecting() {
        let cfg = HooksConfig::default();
        let d = TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).unwrap();
        let err = d
            .channel_for("http://198.51.100.1:9090")
            .await
            .expect_err("plaintext endpoint must be refused");
        assert!(
            matches!(err, HookError::InsecureEndpoint { .. }),
            "expected InsecureEndpoint, got {err:?}"
        );
    }
}
