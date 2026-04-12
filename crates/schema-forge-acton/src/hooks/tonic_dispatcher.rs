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
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Buf, BufMut};
use http::uri::PathAndQuery;
use prost_reflect::prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, Kind, MessageDescriptor, ReflectMessage, Value,
};
use schema_forge_core::types::{DynamicValue, HookEvent};
use tokio::sync::Mutex;
use tonic::client::Grpc;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};
use tracing::{debug, warn};

use super::{HookBinding, HookDispatcher, HookError, HookInvocation, HookOutcome, HooksConfig};

/// Configuration knobs that influence dispatcher construction (timeouts
/// for the channel, descriptor loader, etc.). Distinct from
/// [`HooksConfig`] which describes per-binding policy.
#[derive(Debug, Clone)]
pub struct TonicDispatcherConfig {
    /// Connect timeout applied when opening a tonic [`Channel`] to a hook
    /// endpoint. Defaults to 2 seconds.
    pub connect_timeout: Duration,
}

impl Default for TonicDispatcherConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(2),
        }
    }
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
            let path = binding.descriptor_path.as_deref().ok_or_else(|| {
                HookError::Internal {
                    message: format!(
                        "binding {schema}/{event:?} has no descriptor_path",
                        schema = binding.schema,
                        event = binding.event
                    ),
                }
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

        Ok(Self {
            config: dispatcher,
            bindings,
            channels: Mutex::new(HashMap::new()),
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
        let ep = Endpoint::from_str(endpoint).map_err(|e| HookError::Internal {
            message: format!("invalid endpoint {endpoint}: {e}"),
        })?;
        let ep = ep.connect_timeout(self.config.connect_timeout);
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
        let timeout = binding.timeout_ms.unwrap_or(5000);
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
        let timeout = binding.timeout_ms.unwrap_or(5000);
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
    let path = PathAndQuery::from_maybe_shared(path_str.clone()).map_err(|e| {
        HookError::Internal {
            message: format!("invalid grpc path `{path_str}`: {e}"),
        }
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
        match name {
            "operation" => {
                msg.set_field_by_name(name, Value::String(invocation.operation.clone()));
            }
            "user_id" => {
                if let Some(uid) = &invocation.user_id {
                    msg.set_field_by_name(name, Value::String(uid.clone()));
                }
            }
            "entity_id" => {
                if let Some(eid) = &invocation.entity_id {
                    msg.set_field_by_name(name, Value::String(eid.clone()));
                }
            }
            _ => {
                if let Some(dv) = invocation.fields.get(name) {
                    if let Some(v) = dynamic_value_to_proto(dv, &field.kind()) {
                        msg.set_field_by_name(name, v);
                    }
                }
            }
        }
    }

    Ok(msg)
}

fn dynamic_value_to_proto(value: &DynamicValue, kind: &Kind) -> Option<Value> {
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
        DynamicValue::RefArray(ids) => Some(Value::List(
            ids.iter().map(|i| Value::String(i.to_string())).collect(),
        )),
        DynamicValue::Array(arr) => Some(Value::List(
            arr.iter()
                .filter_map(|v| dynamic_value_to_proto(v, kind))
                .collect(),
        )),
        DynamicValue::Composite(_) => None,
        _ => None,
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

    #[test]
    fn event_method_names() {
        assert_eq!(event_to_method(HookEvent::BeforeChange), "BeforeChange");
        assert_eq!(event_to_method(HookEvent::AfterDelete), "AfterDelete");
    }

    #[test]
    fn dynamic_value_text_to_string_kind() {
        let v = dynamic_value_to_proto(
            &DynamicValue::Text("hi".into()),
            &Kind::String,
        );
        assert!(matches!(v, Some(Value::String(s)) if s == "hi"));
    }

    #[test]
    fn dynamic_value_integer_to_int64() {
        let v = dynamic_value_to_proto(&DynamicValue::Integer(42), &Kind::Int64);
        assert!(matches!(v, Some(Value::I64(42))));
    }

    #[test]
    fn dynamic_value_null_skipped() {
        assert!(dynamic_value_to_proto(&DynamicValue::Null, &Kind::String).is_none());
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
                endpoint: "http://x".into(),
                timeout_ms: None,
                required: false,
                descriptor_path: None,
            }],
            ..HooksConfig::default()
        };
        let err = TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).unwrap_err();
        assert!(matches!(err, HookError::Internal { .. }));
    }
}

