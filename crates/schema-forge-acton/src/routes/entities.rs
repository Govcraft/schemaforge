use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use schema_forge_backend::entity::Entity;
use schema_forge_core::query::{validate_filter, FieldPath, Filter, SortOrder};
use schema_forge_core::types::{
    Cardinality, DynamicValue, EntityId, FieldType, SchemaDefinition, SchemaName,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::instrument;

use super::query_params::{parse_fields_param, parse_filter_params, parse_sort_param};
use crate::access::{
    check_schema_access, filter_entity_fields, inject_tenant_on_create, inject_tenant_scope,
    AccessAction, FieldFilterDirection, OptionalClaims,
};
use crate::actor::ForgeActor;
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;
use crate::hooks::{run_after_hook, run_before_hook, HookDispatcher, HookInvocation, HooksConfig};
use crate::messages::{
    CreateEntity, DeleteEntity, GetEntity, GetHookDispatcher, GetRecordAccessPolicy, GetSchema,
    GetTenantConfig, QueryEntities, ReplyChannel, UpdateEntity,
};
use schema_forge_core::types::HookEvent;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Actor request helper
// ---------------------------------------------------------------------------

/// Timeout for actor request-response round-trips.
const ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

/// Await an actor response with a timeout.
///
/// Wraps the common pattern of awaiting a `oneshot::Receiver` with a deadline
/// and mapping both timeout and channel-dropped errors to `ForgeError::Internal`.
async fn ask_forge<T>(rx: oneshot::Receiver<T>) -> Result<T, ForgeError> {
    tokio::time::timeout(ACTOR_TIMEOUT, rx)
        .await
        .map_err(|_| ForgeError::Internal {
            message: "forge actor timeout".into(),
        })?
        .map_err(|_| ForgeError::Internal {
            message: "forge actor unavailable".into(),
        })
}

// ---------------------------------------------------------------------------
// Hook dispatch helpers
// ---------------------------------------------------------------------------

/// Retrieve the hook dispatcher from the actor, or `None` if hooks are
/// globally disabled or no dispatcher was wired at init time. Returns
/// `None` (not an error) so callers can short-circuit with no extra cost
/// on the hot path when hooks are not in use.
async fn fetch_hook_dispatcher(
    forge: &acton_service::prelude::ActorHandle,
) -> Option<Arc<dyn HookDispatcher>> {
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetHookDispatcher {
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await.ok().flatten()
}

/// Build a `HookInvocation` for the given schema, event, operation, and
/// field snapshot.
fn build_invocation(
    schema: &SchemaDefinition,
    event: HookEvent,
    operation: &str,
    user: Option<&Claims>,
    entity_id: Option<String>,
    fields: &BTreeMap<String, DynamicValue>,
) -> HookInvocation {
    HookInvocation {
        schema: schema.name.as_str().to_string(),
        event,
        operation: operation.to_string(),
        user_id: user.map(|c| c.sub.clone()),
        entity_id,
        fields: fields.clone(),
    }
}

/// Context bundle for [`apply_before_hook`]. Groups the per-request
/// parameters that are constant across a hook call so the helper does
/// not balloon into a wide argument list.
struct BeforeHookCtx<'a> {
    dispatcher: &'a dyn HookDispatcher,
    hooks_config: &'a HooksConfig,
    schema: &'a SchemaDefinition,
    event: HookEvent,
    operation: &'a str,
    user: Option<&'a Claims>,
    entity_id: Option<String>,
}

/// Run a `before_*` hook on a mutable field map. If the hook returns
/// modified fields, replaces the entries in-place. Propagates aborts as
/// [`ForgeError::HookAborted`] and required-hook errors as
/// [`ForgeError::HookUnavailable`].
async fn apply_before_hook(
    ctx: BeforeHookCtx<'_>,
    fields: &mut BTreeMap<String, DynamicValue>,
) -> Result<(), ForgeError> {
    let invocation = build_invocation(
        ctx.schema,
        ctx.event,
        ctx.operation,
        ctx.user,
        ctx.entity_id,
        fields,
    );
    let outcome = run_before_hook(ctx.dispatcher, ctx.hooks_config, invocation)
        .await
        .map_err(ForgeError::from)?;
    if let Some(outcome) = outcome {
        if let Some(modified) = outcome.modified_fields {
            for (k, v) in modified {
                let coerced = match ctx.schema.field(&k) {
                    Some(field_def) => {
                        coerce_dynamic_value_with_type_hint(v, &field_def.field_type).map_err(
                            |reason| ForgeError::HookAborted {
                                reason: format!(
                            "hook returned invalid value for field '{k}' of type {}: {reason}",
                            field_def.field_type
                        ),
                            },
                        )?
                    }
                    // Unknown field name from hook: pass through. Backend
                    // validation already rejects unknown fields, so behavior
                    // is unchanged from before this coercion was added.
                    None => v,
                };
                fields.insert(k, coerced);
            }
        }
    }
    Ok(())
}

/// Run a blocking read hook (`before_read` or `after_read`) using the
/// `before` dispatch path. Read-side hooks need to be blocking so the
/// route handler can apply field-level modifications (e.g. redaction)
/// or honor an abort. Both events route through `dispatcher.call_before`
/// to share the modify/abort response shape.
///
/// Returns `Ok(())` if the hook is not configured or if it ran cleanly.
/// Mutates `fields` in place when the hook returns modifications.
async fn apply_read_hook(
    ctx: BeforeHookCtx<'_>,
    fields: &mut BTreeMap<String, DynamicValue>,
) -> Result<(), ForgeError> {
    // Early exit when this schema has no hook for this specific event —
    // avoids paying any per-call cost in the common case.
    if ctx.schema.hook_for(ctx.event).is_none() {
        return Ok(());
    }
    apply_before_hook(ctx, fields).await
}

/// Owned parameter bundle for [`fire_after_hook`]. Everything is moved
/// into a spawned task, so each field is `'static`-friendly (owned).
struct AfterHookCtx {
    dispatcher: Arc<dyn HookDispatcher>,
    hooks_config: HooksConfig,
    schema: SchemaDefinition,
    event: HookEvent,
    operation: String,
    user_id: Option<String>,
}

/// Dispatch an `after_*` hook. Fire-and-forget: never errors into the
/// caller's response. Honors the global `enabled` flag and per-schema
/// binding presence via [`run_after_hook`].
fn fire_after_hook(ctx: AfterHookCtx, entity: &Entity) {
    let invocation = HookInvocation {
        schema: ctx.schema.name.as_str().to_string(),
        event: ctx.event,
        operation: ctx.operation,
        user_id: ctx.user_id,
        entity_id: Some(entity.id.as_str().to_string()),
        fields: entity.fields.clone(),
    };
    // Spawn into background so the response path never blocks on the
    // after-hook call. The dispatcher is itself responsible for
    // bounding internal concurrency.
    let dispatcher = ctx.dispatcher;
    let hooks_config = ctx.hooks_config;
    tokio::spawn(async move {
        run_after_hook(dispatcher.as_ref(), &hooks_config, invocation).await;
    });
}

// ---------------------------------------------------------------------------
// Webhook dispatch helper
// ---------------------------------------------------------------------------

/// Fire webhook notifications for a CRUD event, if the schema has webhooks enabled.
///
/// This is non-blocking: webhook delivery happens in background tasks.
async fn dispatch_webhook(
    state: &AppState<SchemaForgeConfig>,
    schema_def: &SchemaDefinition,
    event: crate::webhook::WebhookEvent,
    event_type: &str,
) {
    let webhook_config = &state.config().custom.schema_forge.webhooks;
    let dispatcher = match crate::webhook::get_dispatcher(webhook_config) {
        Some(d) => d,
        None => return,
    };

    if !schema_def.has_webhooks() {
        return;
    }
    if !schema_def.webhook_events().contains(&event_type) {
        return;
    }

    // Resolve subscriptions: DSL inline + runtime (via actor query)
    let mut subs = Vec::new();

    // 1. Inline DSL subscription
    if let Some(schema_forge_core::types::Annotation::Webhook {
        url: Some(url),
        secret,
        ..
    }) = schema_def.webhook_annotation()
    {
        subs.push(crate::webhook::ResolvedSubscription {
            url: url.clone(),
            secret: secret.clone(),
            retry_count: None,
            timeout_seconds: None,
        });
    }

    // 2. Runtime subscriptions via actor query
    let forge = match state.actor::<ForgeActor>() {
        Some(f) => f,
        None => {
            if !subs.is_empty() {
                dispatcher.dispatch(event, subs);
            }
            return;
        }
    };

    // Query WebhookSubscription entities for this schema
    let (tx, rx) = oneshot::channel();
    let ws_schema = SchemaName::new("WebhookSubscription");
    if let Ok(ws_name) = ws_schema {
        // First get the WebhookSubscription schema def to get its ID
        let (stx, srx) = oneshot::channel();
        forge
            .send(crate::messages::GetSchema {
                name: ws_name.as_str().to_string(),
                reply: ReplyChannel::new(stx),
            })
            .await;

        if let Ok(Some(ws_def)) = ask_forge(srx).await {
            use schema_forge_core::query::{FieldPath, Filter, Query as CoreQuery};
            let query = CoreQuery::new(ws_def.id.clone()).with_filter(Filter::and(vec![
                Filter::eq(
                    FieldPath::parse("target_schema").unwrap(),
                    DynamicValue::Text(schema_def.name.as_str().to_string()),
                ),
                Filter::eq(
                    FieldPath::parse("active").unwrap(),
                    DynamicValue::Boolean(true),
                ),
            ]));

            forge
                .send(QueryEntities {
                    query,
                    reply: ReplyChannel::new(tx),
                })
                .await;

            if let Ok(Ok(result)) = ask_forge(rx).await {
                for entity in &result.entities {
                    if let Some(sub) = resolve_runtime_subscription(entity, event_type) {
                        subs.push(sub);
                    }
                }
            }
        }
    }

    if !subs.is_empty() {
        dispatcher.dispatch(event, subs);
    }
}

/// Convert a WebhookSubscription entity to a ResolvedSubscription.
fn resolve_runtime_subscription(
    entity: &Entity,
    event_type: &str,
) -> Option<crate::webhook::ResolvedSubscription> {
    // Check event filter
    if let Some(DynamicValue::Array(events)) = entity.fields.get("events") {
        if !events.is_empty()
            && !events
                .iter()
                .any(|e| matches!(e, DynamicValue::Text(t) if t == event_type))
        {
            return None;
        }
    }

    let url = match entity.fields.get("url") {
        Some(DynamicValue::Text(u)) => u.clone(),
        _ => return None,
    };
    let secret = match entity.fields.get("secret") {
        Some(DynamicValue::Text(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let retry_count = match entity.fields.get("retry_count") {
        Some(DynamicValue::Integer(n)) => Some(*n as u32),
        _ => None,
    };
    let timeout_seconds = match entity.fields.get("timeout_seconds") {
        Some(DynamicValue::Integer(n)) => Some(*n as u32),
        _ => None,
    };

    Some(crate::webhook::ResolvedSubscription {
        url,
        secret,
        retry_count,
        timeout_seconds,
    })
}

// ---------------------------------------------------------------------------
// Request/Response types
// ---------------------------------------------------------------------------

/// Request body for creating/updating an entity.
#[derive(Debug, Deserialize)]
pub struct EntityRequest {
    /// The entity fields as a JSON map.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Response for a single entity.
#[derive(Debug, Serialize)]
pub struct EntityResponse {
    /// The entity ID.
    pub id: String,
    /// The schema name.
    pub schema: String,
    /// The entity fields.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Response for entity list/query.
#[derive(Debug, Serialize)]
pub struct ListEntitiesResponse {
    /// The entities.
    pub entities: Vec<EntityResponse>,
    /// The count of entities in this response.
    pub count: usize,
    /// The total count of matching entities before pagination, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<usize>,
}

/// Request body for POST query endpoint.
#[derive(Debug, Deserialize)]
pub struct EntityQueryBody {
    /// Raw JSON filter — converted to `Filter` using schema type hints.
    #[serde(default)]
    pub filter: Option<serde_json::Value>,
    /// Sort clauses.
    #[serde(default)]
    pub sort: Option<Vec<SortClause>>,
    /// Maximum number of entities to return.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of entities to skip.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Field projection — only return these fields in the response.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

/// A single sort clause in a POST query body.
#[derive(Debug, Deserialize)]
pub struct SortClause {
    /// Field name (supports dotted paths).
    pub field: String,
    /// Sort direction: "asc" or "desc". Defaults to "asc".
    #[serde(default)]
    pub order: Option<String>,
}

// ---------------------------------------------------------------------------
// Conversion helpers (pure functions)
// ---------------------------------------------------------------------------

/// Convert a JSON field map to `DynamicValue` fields using schema type information.
///
/// Pure function: no I/O. Returns a list of validation errors if any fields
/// fail type conversion.
pub fn json_to_entity_fields(
    schema: &SchemaDefinition,
    json_fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<BTreeMap<String, DynamicValue>, Vec<String>> {
    let mut fields = BTreeMap::new();
    let mut errors = Vec::new();

    for (key, value) in json_fields {
        // Look up the field type in the schema for guidance
        let field_def = schema.field(key);

        let dynamic_value = if let Some(def) = field_def {
            convert_json_with_type_hint(value, &def.field_type)
        } else {
            // Unknown field -- convert based on JSON type
            convert_json_untyped(value)
        };

        match dynamic_value {
            Ok(dv) => {
                fields.insert(key.clone(), dv);
            }
            Err(msg) => {
                errors.push(format!("field '{key}': {msg}"));
            }
        }
    }

    // Check for required fields that are missing
    for field_def in &schema.fields {
        if field_def.is_required() && !json_fields.contains_key(field_def.name.as_str()) {
            errors.push(format!(
                "required field '{}' is missing",
                field_def.name.as_str()
            ));
        }
    }

    if errors.is_empty() {
        Ok(fields)
    } else {
        Err(errors)
    }
}

/// Convert a JSON value to a DynamicValue using the field type as a hint.
fn convert_json_with_type_hint(
    value: &serde_json::Value,
    field_type: &FieldType,
) -> Result<DynamicValue, String> {
    match field_type {
        FieldType::Text(_) | FieldType::RichText => match value {
            serde_json::Value::String(s) => Ok(DynamicValue::Text(s.clone())),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            other => Ok(DynamicValue::Text(other.to_string())),
        },
        FieldType::Integer(_) => match value {
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(DynamicValue::Integer)
                .ok_or_else(|| format!("expected integer, got {value}")),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected integer, got {value}")),
        },
        FieldType::Float(_) => match value {
            serde_json::Value::Number(n) => n
                .as_f64()
                .map(DynamicValue::Float)
                .ok_or_else(|| format!("expected float, got {value}")),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected float, got {value}")),
        },
        FieldType::Boolean => match value {
            serde_json::Value::Bool(b) => Ok(DynamicValue::Boolean(*b)),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected boolean, got {value}")),
        },
        FieldType::DateTime => match value {
            serde_json::Value::String(s) => {
                // Try to parse as a DateTime
                s.parse::<chrono::DateTime<chrono::Utc>>()
                    .map(DynamicValue::DateTime)
                    .map_err(|e| format!("invalid datetime '{s}': {e}"))
            }
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected datetime string, got {value}")),
        },
        FieldType::Enum(_) => match value {
            serde_json::Value::String(s) => Ok(DynamicValue::Enum(s.clone())),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected enum string, got {value}")),
        },
        FieldType::Json => Ok(DynamicValue::Json(value.clone())),
        FieldType::Relation {
            target: _,
            cardinality,
        } => match value {
            serde_json::Value::String(s) => {
                let entity_id = EntityId::parse(s)
                    .map_err(|e| format!("invalid entity reference '{s}': {e}"))?;
                Ok(DynamicValue::Ref(entity_id))
            }
            serde_json::Value::Array(arr) if matches!(cardinality, Cardinality::Many) => {
                let ids = arr
                    .iter()
                    .map(|v| {
                        if let serde_json::Value::String(s) = v {
                            EntityId::parse(s)
                                .map_err(|e| format!("invalid entity reference '{s}': {e}"))
                        } else {
                            Err(format!("expected string entity reference, got {v}"))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DynamicValue::RefArray(ids))
            }
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected entity reference string, got {value}")),
        },
        FieldType::Array(inner) => match value {
            serde_json::Value::Array(arr) => {
                let items = arr
                    .iter()
                    .map(|v| convert_json_with_type_hint(v, inner))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DynamicValue::Array(items))
            }
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected array, got {value}")),
        },
        _ => convert_json_untyped(value),
    }
}

/// Convert a JSON value to a DynamicValue without type hints.
fn convert_json_untyped(value: &serde_json::Value) -> Result<DynamicValue, String> {
    match value {
        serde_json::Value::Null => Ok(DynamicValue::Null),
        serde_json::Value::Bool(b) => Ok(DynamicValue::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(DynamicValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(DynamicValue::Float(f))
            } else {
                Ok(DynamicValue::Text(n.to_string()))
            }
        }
        serde_json::Value::String(s) => Ok(DynamicValue::Text(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr.iter().map(convert_json_untyped).collect();
            Ok(DynamicValue::Array(items?))
        }
        serde_json::Value::Object(map) => {
            let mut btree = BTreeMap::new();
            for (k, v) in map {
                btree.insert(k.clone(), convert_json_untyped(v)?);
            }
            Ok(DynamicValue::Composite(btree))
        }
    }
}

/// Coerce a [`DynamicValue`] against a schema [`FieldType`], parsing
/// stringly-typed values into their typed counterparts.
///
/// Mirrors [`convert_json_with_type_hint`], but operates on `DynamicValue`
/// inputs. The primary use case is the hook dispatcher's response-merge
/// step: gRPC responses deliver `datetime`/`enum`/`relation` fields as
/// proto `string` (per the wire contract in `docs/hooks-reference.md`
/// §3.4), which arrives here as [`DynamicValue::Text`]. Without this
/// coercion, text values are bound against typed Postgres columns and
/// the write fails with a type mismatch.
fn coerce_dynamic_value_with_type_hint(
    value: DynamicValue,
    field_type: &FieldType,
) -> Result<DynamicValue, String> {
    match field_type {
        FieldType::Text(_) | FieldType::RichText => match value {
            DynamicValue::Text(_) | DynamicValue::Null => Ok(value),
            other => Ok(DynamicValue::Text(other.to_string())),
        },
        FieldType::Integer(_) => match value {
            DynamicValue::Integer(_) | DynamicValue::Null => Ok(value),
            DynamicValue::Text(s) => s
                .parse::<i64>()
                .map(DynamicValue::Integer)
                .map_err(|e| format!("invalid integer '{s}': {e}")),
            other => Err(format!("expected integer, got {other}")),
        },
        FieldType::Float(_) => match value {
            DynamicValue::Float(_) | DynamicValue::Null => Ok(value),
            DynamicValue::Integer(i) => Ok(DynamicValue::Float(i as f64)),
            DynamicValue::Text(s) => s
                .parse::<f64>()
                .map(DynamicValue::Float)
                .map_err(|e| format!("invalid float '{s}': {e}")),
            other => Err(format!("expected float, got {other}")),
        },
        FieldType::Boolean => match value {
            DynamicValue::Boolean(_) | DynamicValue::Null => Ok(value),
            DynamicValue::Text(s) => s
                .parse::<bool>()
                .map(DynamicValue::Boolean)
                .map_err(|e| format!("invalid boolean '{s}': {e}")),
            other => Err(format!("expected boolean, got {other}")),
        },
        FieldType::DateTime => match value {
            DynamicValue::DateTime(_) | DynamicValue::Null => Ok(value),
            DynamicValue::Text(s) => s
                .parse::<chrono::DateTime<chrono::Utc>>()
                .map(DynamicValue::DateTime)
                .map_err(|e| format!("invalid datetime '{s}': {e}")),
            other => Err(format!("expected datetime, got {other}")),
        },
        FieldType::Enum(_) => match value {
            DynamicValue::Enum(_) | DynamicValue::Null => Ok(value),
            DynamicValue::Text(s) => Ok(DynamicValue::Enum(s)),
            other => Err(format!("expected enum, got {other}")),
        },
        FieldType::Json => Ok(value),
        FieldType::Relation { cardinality, .. } => match value {
            DynamicValue::Ref(_) | DynamicValue::RefArray(_) | DynamicValue::Null => Ok(value),
            DynamicValue::Text(s) => EntityId::parse(&s)
                .map(DynamicValue::Ref)
                .map_err(|e| format!("invalid entity reference '{s}': {e}")),
            DynamicValue::Array(items) if matches!(cardinality, Cardinality::Many) => {
                let ids = items
                    .into_iter()
                    .map(|item| match item {
                        DynamicValue::Text(s) => EntityId::parse(&s)
                            .map_err(|e| format!("invalid entity reference '{s}': {e}")),
                        DynamicValue::Ref(id) => Ok(id),
                        other => Err(format!("expected entity reference, got {other}")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DynamicValue::RefArray(ids))
            }
            other => Err(format!("expected entity reference, got {other}")),
        },
        FieldType::Array(inner) => match value {
            DynamicValue::Array(items) => {
                let coerced = items
                    .into_iter()
                    .map(|item| coerce_dynamic_value_with_type_hint(item, inner))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DynamicValue::Array(coerced))
            }
            DynamicValue::Null => Ok(DynamicValue::Null),
            other => Err(format!("expected array, got {other}")),
        },
        // Composite fields are passed through unchanged. Nested datetime
        // coercion over composite structures is not exercised by any
        // in-repo schema today; add recursion here if/when needed.
        FieldType::Composite(_) => Ok(value),
        // `FieldType` is `#[non_exhaustive]`; future variants pass through.
        _ => Ok(value),
    }
}

/// Convert an `Entity` to an `EntityResponse`.
fn entity_to_response(entity: &Entity) -> EntityResponse {
    crate::conversions::entity_to_response(entity)
}

/// Convert a `DynamicValue` to a JSON value.
#[cfg(test)]
fn dynamic_value_to_json(value: &DynamicValue) -> serde_json::Value {
    crate::conversions::dynamic_value_to_json(value)
}

/// Validate and parse a schema name from a path parameter.
fn validate_schema_name(name: &str) -> Result<SchemaName, ForgeError> {
    SchemaName::new(name).map_err(|_| ForgeError::InvalidSchemaName {
        name: name.to_string(),
    })
}

/// Convert a raw JSON value into a `Filter` using schema type hints.
///
/// Accepts a JSON object with `"op"`, `"field"`, `"value"` / `"values"` / `"filters"` / `"filter"` keys.
/// Values are plain JSON primitives — types are inferred from schema field definitions.
pub fn json_to_filter(
    value: &serde_json::Value,
    schema: &SchemaDefinition,
) -> Result<Filter, Vec<String>> {
    let obj = value
        .as_object()
        .ok_or_else(|| vec!["filter must be a JSON object".to_string()])?;

    let op = obj
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| vec!["filter must have an 'op' field".to_string()])?;

    match op {
        "and" | "or" => {
            let filters_val = obj
                .get("filters")
                .ok_or_else(|| vec![format!("'{op}' filter must have a 'filters' array")])?;
            let arr = filters_val
                .as_array()
                .ok_or_else(|| vec![format!("'{op}' filter 'filters' must be an array")])?;
            let mut filters = Vec::new();
            let mut errors = Vec::new();
            for item in arr {
                match json_to_filter(item, schema) {
                    Ok(f) => filters.push(f),
                    Err(errs) => errors.extend(errs),
                }
            }
            if !errors.is_empty() {
                return Err(errors);
            }
            Ok(if op == "and" {
                Filter::and(filters)
            } else {
                Filter::or(filters)
            })
        }
        "not" => {
            let inner = obj
                .get("filter")
                .ok_or_else(|| vec!["'not' filter must have a 'filter' field".to_string()])?;
            let f = json_to_filter(inner, schema)?;
            Ok(Filter::negate(f))
        }
        // Leaf operators
        "eq" | "ne" | "gt" | "gte" | "lt" | "lte" | "contains" | "startswith" | "in" => {
            let field_str = obj
                .get("field")
                .and_then(|v| v.as_str())
                .ok_or_else(|| vec![format!("'{op}' filter must have a 'field' string")])?;
            let path = FieldPath::parse(field_str)
                .map_err(|e| vec![format!("invalid field path '{field_str}': {e}")])?;
            let field_type = schema.field(path.root()).map(|fd| &fd.field_type);

            match op {
                "contains" => {
                    let val = obj.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                        vec!["'contains' filter 'value' must be a string".to_string()]
                    })?;
                    Ok(Filter::contains(path, val))
                }
                "startswith" => {
                    let val = obj.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                        vec!["'startswith' filter 'value' must be a string".to_string()]
                    })?;
                    Ok(Filter::starts_with(path, val))
                }
                "in" => {
                    let values_val =
                        obj.get("values")
                            .or_else(|| obj.get("value"))
                            .ok_or_else(|| {
                                vec!["'in' filter must have a 'values' array".to_string()]
                            })?;
                    let arr = values_val
                        .as_array()
                        .ok_or_else(|| vec!["'in' filter 'values' must be an array".to_string()])?;
                    let mut values = Vec::new();
                    let mut errors = Vec::new();
                    for item in arr {
                        match coerce_json_filter_value(item, field_type) {
                            Ok(v) => values.push(v),
                            Err(e) => errors.push(format!("field '{field_str}': {e}")),
                        }
                    }
                    if !errors.is_empty() {
                        return Err(errors);
                    }
                    Ok(Filter::in_set(path, values))
                }
                _ => {
                    // eq, ne, gt, gte, lt, lte
                    let raw = obj
                        .get("value")
                        .ok_or_else(|| vec![format!("'{op}' filter must have a 'value' field")])?;
                    let dv = coerce_json_filter_value(raw, field_type)
                        .map_err(|e| vec![format!("field '{field_str}': {e}")])?;
                    Ok(match op {
                        "eq" => Filter::eq(path, dv),
                        "ne" => Filter::ne(path, dv),
                        "gt" => Filter::gt(path, dv),
                        "gte" => Filter::gte(path, dv),
                        "lt" => Filter::lt(path, dv),
                        "lte" => Filter::lte(path, dv),
                        _ => unreachable!(),
                    })
                }
            }
        }
        _ => Err(vec![format!("unknown filter operator '{op}'")]),
    }
}

/// Coerce a JSON value to a `DynamicValue` for use in filter expressions.
///
/// Uses the field type hint when available, otherwise falls back to untyped conversion.
fn coerce_json_filter_value(
    value: &serde_json::Value,
    field_type: Option<&FieldType>,
) -> Result<DynamicValue, String> {
    if let Some(ft) = field_type {
        convert_json_with_type_hint(value, ft)
    } else {
        convert_json_untyped(value)
    }
}

/// Execute a query with the standard access-control pipeline.
///
/// Shared by `list_entities` and `query_entities`. Sends backend queries
/// through the actor via oneshot channels.
async fn execute_entity_query(
    state: &AppState<SchemaForgeConfig>,
    schema_def: &SchemaDefinition,
    claims: Option<&Claims>,
    query: &mut schema_forge_core::query::Query,
    projection: Option<&HashSet<String>>,
) -> Result<ListEntitiesResponse, ForgeError> {
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Inject tenant scope filter
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetTenantConfig {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let tenant_config = ask_forge(rx).await?;
    inject_tenant_scope(query, claims, &tenant_config);

    // Push field projection into the query for DB-level column selection
    if let Some(proj) = projection {
        query.projection = Some(proj.iter().cloned().collect());
    }

    // Execute query via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(QueryEntities {
            query: query.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let result = ask_forge(rx).await?.map_err(ForgeError::from)?;

    // Record-level access filtering (e.g. @owner)
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetRecordAccessPolicy {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let record_access_policy = ask_forge(rx).await?;

    let visible_entities = if let (Some(ref policy), Some(c)) = (&record_access_policy, claims) {
        policy.filter_visible(schema_def, c, result.entities).await
    } else {
        result.entities
    };

    // Filter read-restricted fields, then apply optional field projection
    let entities: Vec<EntityResponse> = visible_entities
        .into_iter()
        .map(|mut e| {
            filter_entity_fields(&mut e, schema_def, claims, FieldFilterDirection::Read);
            if let Some(proj) = projection {
                e.fields.retain(|k, _| proj.contains(k));
            }
            entity_to_response(&e)
        })
        .collect();
    let count = entities.len();

    Ok(ListEntitiesResponse {
        entities,
        count,
        total_count: result.total_count,
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /schemas/{schema}/entities -- Create a new entity.
#[instrument(skip_all, fields(schema = %schema))]
pub async fn create_entity(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Look up schema via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Access check
    if let Err(e) = check_schema_access(&schema_def, claims.as_ref(), AccessAction::Write) {
        if let Some(logger) = state.audit_logger() {
            logger
                .log_custom(
                    "forge.access.denied",
                    acton_service::audit::AuditSeverity::Warning,
                    Some(serde_json::json!({
                        "schema": &schema,
                        "action": "write",
                        "user": claims.as_ref().map(|c| &c.sub),
                    })),
                )
                .await;
        }
        return Err(e);
    }

    // Convert JSON fields to DynamicValue fields
    let mut fields = json_to_entity_fields(&schema_def, &body.fields)
        .map_err(|errors| ForgeError::ValidationFailed { details: errors })?;

    // Get tenant config via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetTenantConfig {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let tenant_config = ask_forge(rx).await?;
    inject_tenant_on_create(&mut fields, claims.as_ref(), &tenant_config);

    // before_change hook
    let hooks_config = state.config().custom.schema_forge.hooks.clone();
    let hook_dispatcher = if hooks_config.enabled && schema_def.has_hooks() {
        fetch_hook_dispatcher(forge).await
    } else {
        None
    };
    if let Some(ref dispatcher) = hook_dispatcher {
        apply_before_hook(
            BeforeHookCtx {
                dispatcher: dispatcher.as_ref(),
                hooks_config: &hooks_config,
                schema: &schema_def,
                event: HookEvent::BeforeChange,
                operation: "create",
                user: claims.as_ref(),
                entity_id: None,
            },
            &mut fields,
        )
        .await?;
    }

    // Create the entity, filtering write-restricted fields
    let mut entity = Entity::new(schema_name, fields);
    filter_entity_fields(
        &mut entity,
        &schema_def,
        claims.as_ref(),
        FieldFilterDirection::Write,
    );

    // Create entity via actor (supervised backend call)
    let (tx, rx) = oneshot::channel();
    forge
        .send(CreateEntity {
            entity,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let mut created = ask_forge(rx).await?.map_err(ForgeError::from)?;

    // after_change hook (fire-and-forget)
    if let Some(dispatcher) = hook_dispatcher.clone() {
        fire_after_hook(
            AfterHookCtx {
                dispatcher,
                hooks_config: hooks_config.clone(),
                schema: schema_def.clone(),
                event: HookEvent::AfterChange,
                operation: "create".to_string(),
                user_id: claims.as_ref().map(|c| c.sub.clone()),
            },
            &created,
        );
    }

    // Filter read-restricted fields from response
    filter_entity_fields(
        &mut created,
        &schema_def,
        claims.as_ref(),
        FieldFilterDirection::Read,
    );

    // Audit: entity created
    if let Some(logger) = state.audit_logger() {
        logger
            .log_custom(
                "forge.entity.created",
                acton_service::audit::AuditSeverity::Informational,
                Some(serde_json::json!({
                    "schema": schema,
                    "entity_id": created.id.as_str(),
                    "user": claims.as_ref().map(|c| &c.sub),
                })),
            )
            .await;
    }

    // Webhook: fire notifications
    let webhook_event = crate::webhook::WebhookEvent::from_create(
        &schema,
        &created,
        claims.as_ref().map(|c| c.sub.as_str()),
    );
    dispatch_webhook(&state, &schema_def, webhook_event, "created").await;

    Ok((StatusCode::CREATED, Json(entity_to_response(&created))))
}

/// GET /schemas/{schema}/entities -- List/query entities.
///
/// Supports filter, sort, limit, and offset via query parameters.
/// Filter params use Django-style syntax: `?field__op=value` (e.g. `?age__gt=25`).
/// Sort uses `?sort=-age,name` (prefix `-` = descending) or `?sort=age:desc,name:asc`.
#[instrument(skip_all, fields(schema = %schema))]
pub async fn list_entities(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Verify schema exists via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Access check
    check_schema_access(&schema_def, claims.as_ref(), AccessAction::Read)?;

    // before_read hook gate (no entity_id, no fields — list scope).
    let hooks_config = state.config().custom.schema_forge.hooks.clone();
    if hooks_config.enabled && schema_def.hook_for(HookEvent::BeforeRead).is_some() {
        if let Some(dispatcher) = fetch_hook_dispatcher(forge).await {
            let mut empty = BTreeMap::new();
            apply_read_hook(
                BeforeHookCtx {
                    dispatcher: dispatcher.as_ref(),
                    hooks_config: &hooks_config,
                    schema: &schema_def,
                    event: HookEvent::BeforeRead,
                    operation: "list",
                    user: claims.as_ref(),
                    entity_id: None,
                },
                &mut empty,
            )
            .await?;
        }
    }

    // Build a query
    let mut query = schema_forge_core::query::Query::new(schema_def.id.clone());

    // Extract limit/offset
    if let Some(limit_str) = params.get("limit") {
        let limit = limit_str
            .parse::<usize>()
            .map_err(|_| ForgeError::InvalidQuery {
                message: format!("invalid limit value '{limit_str}'"),
            })?;
        query = query.with_limit(limit);
    }
    if let Some(offset_str) = params.get("offset") {
        let offset = offset_str
            .parse::<usize>()
            .map_err(|_| ForgeError::InvalidQuery {
                message: format!("invalid offset value '{offset_str}'"),
            })?;
        query = query.with_offset(offset);
    }

    // Parse sort
    if let Some(sort_str) = params.get("sort") {
        let sort_clauses =
            parse_sort_param(sort_str).map_err(|e| ForgeError::InvalidQuery { message: e })?;
        for (path, order) in sort_clauses {
            query = query.with_sort(path, order);
        }
    }

    // Parse filter params
    let filter =
        parse_filter_params(&params, &schema_def).map_err(|errors| ForgeError::InvalidQuery {
            message: errors.join("; "),
        })?;
    if let Some(f) = &filter {
        validate_filter(f, &schema_def).map_err(|errors| ForgeError::InvalidQuery {
            message: errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        })?;
    }
    if let Some(f) = filter {
        query = query.with_filter(f);
    }

    // Parse field projection
    let projection = if let Some(fields_str) = params.get("fields") {
        Some(
            parse_fields_param(fields_str, &schema_def)
                .map_err(|e| ForgeError::InvalidQuery { message: e })?,
        )
    } else {
        None
    };

    let response = execute_entity_query(
        &state,
        &schema_def,
        claims.as_ref(),
        &mut query,
        projection.as_ref(),
    )
    .await?;
    Ok(Json(response))
}

/// POST /schemas/{schema}/entities/query -- Advanced query with JSON body.
///
/// Accepts a full filter IR as JSON with plain values (schema-inferred types).
#[instrument(skip_all, fields(schema = %schema))]
pub async fn query_entities(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<EntityQueryBody>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Verify schema exists via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Access check
    check_schema_access(&schema_def, claims.as_ref(), AccessAction::Read)?;

    // before_read hook gate (no entity_id, no fields — query scope).
    let hooks_config = state.config().custom.schema_forge.hooks.clone();
    if hooks_config.enabled && schema_def.hook_for(HookEvent::BeforeRead).is_some() {
        if let Some(dispatcher) = fetch_hook_dispatcher(forge).await {
            let mut empty = BTreeMap::new();
            apply_read_hook(
                BeforeHookCtx {
                    dispatcher: dispatcher.as_ref(),
                    hooks_config: &hooks_config,
                    schema: &schema_def,
                    event: HookEvent::BeforeRead,
                    operation: "query",
                    user: claims.as_ref(),
                    entity_id: None,
                },
                &mut empty,
            )
            .await?;
        }
    }

    // Build a query
    let mut query = schema_forge_core::query::Query::new(schema_def.id.clone());

    if let Some(limit) = body.limit {
        query = query.with_limit(limit);
    }
    if let Some(offset) = body.offset {
        query = query.with_offset(offset);
    }

    // Parse sort clauses
    if let Some(sort_clauses) = &body.sort {
        for clause in sort_clauses {
            let path = FieldPath::parse(&clause.field).map_err(|e| ForgeError::InvalidQuery {
                message: format!("invalid sort field '{}': {e}", clause.field),
            })?;
            let order = match clause.order.as_deref() {
                Some("desc") => SortOrder::Descending,
                Some("asc") | None => SortOrder::Ascending,
                Some(other) => {
                    return Err(ForgeError::InvalidQuery {
                        message: format!("invalid sort order '{other}', expected 'asc' or 'desc'"),
                    });
                }
            };
            query = query.with_sort(path, order);
        }
    }

    // Parse filter
    if let Some(filter_json) = &body.filter {
        let filter = json_to_filter(filter_json, &schema_def).map_err(|errors| {
            ForgeError::InvalidQuery {
                message: errors.join("; "),
            }
        })?;
        validate_filter(&filter, &schema_def).map_err(|errors| ForgeError::InvalidQuery {
            message: errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        })?;
        query = query.with_filter(filter);
    }

    // Validate field projection
    let projection = if let Some(ref field_names) = body.fields {
        let set: HashSet<String> = field_names.iter().cloned().collect();
        let unknown: Vec<&str> = set
            .iter()
            .filter(|n| {
                !schema_def
                    .fields
                    .iter()
                    .any(|f| f.name.as_str() == n.as_str())
            })
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(ForgeError::InvalidQuery {
                message: format!("unknown fields: {}", unknown.join(", ")),
            });
        }
        Some(set)
    } else {
        None
    };

    let response = execute_entity_query(
        &state,
        &schema_def,
        claims.as_ref(),
        &mut query,
        projection.as_ref(),
    )
    .await?;
    Ok(Json(response))
}

/// GET /schemas/{schema}/entities/{id} -- Get entity by ID.
#[instrument(skip_all, fields(schema = %schema))]
pub async fn get_entity(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, id)): Path<(String, String)>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Look up schema via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Access check
    check_schema_access(&schema_def, claims.as_ref(), AccessAction::Read)?;

    // Parse the entity ID
    let entity_id =
        EntityId::parse(&id).map_err(|_| ForgeError::InvalidEntityId { id: id.clone() })?;

    // Resolve hook configuration up front. Only fetch the dispatcher when
    // this schema actually declares a read-side hook — keeps the cost
    // zero for the common case where reads are unhooked.
    let hooks_config = state.config().custom.schema_forge.hooks.clone();
    let read_hook_dispatcher = if hooks_config.enabled
        && (schema_def.hook_for(HookEvent::BeforeRead).is_some()
            || schema_def.hook_for(HookEvent::AfterRead).is_some())
    {
        fetch_hook_dispatcher(forge).await
    } else {
        None
    };

    // before_read hook (blocking; no fields yet — entity has not been fetched).
    if let Some(ref dispatcher) = read_hook_dispatcher {
        let mut empty_fields = BTreeMap::new();
        apply_read_hook(
            BeforeHookCtx {
                dispatcher: dispatcher.as_ref(),
                hooks_config: &hooks_config,
                schema: &schema_def,
                event: HookEvent::BeforeRead,
                operation: "read",
                user: claims.as_ref(),
                entity_id: Some(entity_id.as_str().to_string()),
            },
            &mut empty_fields,
        )
        .await?;
    }

    // Get entity via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetEntity {
            schema: schema_name,
            id: entity_id.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let mut entity = ask_forge(rx).await?.map_err(ForgeError::from)?;

    // Record-level visibility check
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetRecordAccessPolicy {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let record_access_policy = ask_forge(rx).await?;

    if let (Some(ref policy), Some(ref c)) = (&record_access_policy, &claims) {
        let visible = policy
            .filter_visible(&schema_def, c, vec![entity.clone()])
            .await;
        if visible.is_empty() {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to view entity '{id}'"),
            });
        }
    }

    // Filter read-restricted fields from response
    filter_entity_fields(
        &mut entity,
        &schema_def,
        claims.as_ref(),
        FieldFilterDirection::Read,
    );

    // after_read hook (blocking; runs through the same call_before path
    // so it can patch the response payload — e.g. redact, decorate).
    if let Some(ref dispatcher) = read_hook_dispatcher {
        apply_read_hook(
            BeforeHookCtx {
                dispatcher: dispatcher.as_ref(),
                hooks_config: &hooks_config,
                schema: &schema_def,
                event: HookEvent::AfterRead,
                operation: "read",
                user: claims.as_ref(),
                entity_id: Some(entity.id.as_str().to_string()),
            },
            &mut entity.fields,
        )
        .await?;
    }

    Ok(Json(entity_to_response(&entity)))
}

/// PUT /schemas/{schema}/entities/{id} -- Update entity.
#[instrument(skip_all, fields(schema = %schema))]
pub async fn update_entity(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, id)): Path<(String, String)>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Parse the entity ID
    let entity_id =
        EntityId::parse(&id).map_err(|_| ForgeError::InvalidEntityId { id: id.clone() })?;

    // Look up schema via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Access check
    if let Err(e) = check_schema_access(&schema_def, claims.as_ref(), AccessAction::Write) {
        if let Some(logger) = state.audit_logger() {
            logger
                .log_custom(
                    "forge.access.denied",
                    acton_service::audit::AuditSeverity::Warning,
                    Some(serde_json::json!({
                        "schema": &schema,
                        "action": "write",
                        "user": claims.as_ref().map(|c| &c.sub),
                    })),
                )
                .await;
        }
        return Err(e);
    }

    // Record-level ownership check: fetch existing entity and verify ownership
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetRecordAccessPolicy {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let record_access_policy = ask_forge(rx).await?;

    if let (Some(ref policy), Some(ref c)) = (&record_access_policy, &claims) {
        let (tx, rx) = oneshot::channel();
        forge
            .send(GetEntity {
                schema: schema_name.clone(),
                id: entity_id.clone(),
                reply: ReplyChannel::new(tx),
            })
            .await;
        let existing = ask_forge(rx).await?.map_err(ForgeError::from)?;
        if !policy.can_modify(&schema_def, c, &existing).await {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to modify entity '{id}'"),
            });
        }
    }

    // Convert JSON fields
    let mut fields = json_to_entity_fields(&schema_def, &body.fields)
        .map_err(|errors| ForgeError::ValidationFailed { details: errors })?;

    // before_change hook
    let hooks_config = state.config().custom.schema_forge.hooks.clone();
    let hook_dispatcher = if hooks_config.enabled && schema_def.has_hooks() {
        fetch_hook_dispatcher(forge).await
    } else {
        None
    };
    if let Some(ref dispatcher) = hook_dispatcher {
        apply_before_hook(
            BeforeHookCtx {
                dispatcher: dispatcher.as_ref(),
                hooks_config: &hooks_config,
                schema: &schema_def,
                event: HookEvent::BeforeChange,
                operation: "update",
                user: claims.as_ref(),
                entity_id: Some(entity_id.as_str().to_string()),
            },
            &mut fields,
        )
        .await?;
    }

    // Build entity with specific ID, filtering write-restricted fields
    let mut entity = Entity::with_id(entity_id, schema_name, fields);
    filter_entity_fields(
        &mut entity,
        &schema_def,
        claims.as_ref(),
        FieldFilterDirection::Write,
    );

    // Update entity via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(UpdateEntity {
            entity,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let mut updated = ask_forge(rx).await?.map_err(ForgeError::from)?;

    // after_change hook (fire-and-forget)
    if let Some(dispatcher) = hook_dispatcher.clone() {
        fire_after_hook(
            AfterHookCtx {
                dispatcher,
                hooks_config: hooks_config.clone(),
                schema: schema_def.clone(),
                event: HookEvent::AfterChange,
                operation: "update".to_string(),
                user_id: claims.as_ref().map(|c| c.sub.clone()),
            },
            &updated,
        );
    }

    // Filter read-restricted fields from response
    filter_entity_fields(
        &mut updated,
        &schema_def,
        claims.as_ref(),
        FieldFilterDirection::Read,
    );

    // Audit: entity updated
    if let Some(logger) = state.audit_logger() {
        logger
            .log_custom(
                "forge.entity.updated",
                acton_service::audit::AuditSeverity::Informational,
                Some(serde_json::json!({
                    "schema": schema,
                    "entity_id": updated.id.as_str(),
                    "user": claims.as_ref().map(|c| &c.sub),
                })),
            )
            .await;
    }

    // Webhook: fire notifications
    let webhook_event = crate::webhook::WebhookEvent::from_update(
        &schema,
        &updated,
        claims.as_ref().map(|c| c.sub.as_str()),
    );
    dispatch_webhook(&state, &schema_def, webhook_event, "updated").await;

    Ok(Json(entity_to_response(&updated)))
}

/// DELETE /schemas/{schema}/entities/{id} -- Delete entity.
#[instrument(skip_all, fields(schema = %schema))]
pub async fn delete_entity(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, id)): Path<(String, String)>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Look up schema via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Access check
    if let Err(e) = check_schema_access(&schema_def, claims.as_ref(), AccessAction::Delete) {
        if let Some(logger) = state.audit_logger() {
            logger
                .log_custom(
                    "forge.access.denied",
                    acton_service::audit::AuditSeverity::Warning,
                    Some(serde_json::json!({
                        "schema": &schema,
                        "action": "delete",
                        "user": claims.as_ref().map(|c| &c.sub),
                    })),
                )
                .await;
        }
        return Err(e);
    }

    let entity_id =
        EntityId::parse(&id).map_err(|_| ForgeError::InvalidEntityId { id: id.clone() })?;

    // Record-level ownership check: fetch entity first and verify ownership
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetRecordAccessPolicy {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let record_access_policy = ask_forge(rx).await?;

    if let (Some(ref policy), Some(ref c)) = (&record_access_policy, &claims) {
        let (tx, rx) = oneshot::channel();
        forge
            .send(GetEntity {
                schema: schema_name.clone(),
                id: entity_id.clone(),
                reply: ReplyChannel::new(tx),
            })
            .await;
        let entity = ask_forge(rx).await?.map_err(ForgeError::from)?;
        if !policy.can_delete(&schema_def, c, &entity).await {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to delete entity '{id}'"),
            });
        }
    }

    // before_delete / after_delete hook setup. Fetch entity snapshot when a
    // hook is configured so the dispatcher sees the fields being deleted.
    let hooks_config = state.config().custom.schema_forge.hooks.clone();
    let hook_dispatcher = if hooks_config.enabled && schema_def.has_hooks() {
        fetch_hook_dispatcher(forge).await
    } else {
        None
    };
    let mut pre_delete_snapshot: Option<Entity> = None;
    if hook_dispatcher.is_some()
        && (hooks_config
            .binding_for(schema_def.name.as_str(), HookEvent::BeforeDelete)
            .is_some()
            || hooks_config
                .binding_for(schema_def.name.as_str(), HookEvent::AfterDelete)
                .is_some())
    {
        let (tx, rx) = oneshot::channel();
        forge
            .send(GetEntity {
                schema: schema_name.clone(),
                id: entity_id.clone(),
                reply: ReplyChannel::new(tx),
            })
            .await;
        if let Ok(Ok(e)) = ask_forge(rx).await {
            pre_delete_snapshot = Some(e);
        }
    }
    if let (Some(ref dispatcher), Some(snapshot)) = (&hook_dispatcher, &pre_delete_snapshot) {
        let mut fields = snapshot.fields.clone();
        apply_before_hook(
            BeforeHookCtx {
                dispatcher: dispatcher.as_ref(),
                hooks_config: &hooks_config,
                schema: &schema_def,
                event: HookEvent::BeforeDelete,
                operation: "delete",
                user: claims.as_ref(),
                entity_id: Some(entity_id.as_str().to_string()),
            },
            &mut fields,
        )
        .await?;
    }

    // Delete entity via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(DeleteEntity {
            schema: schema_name,
            id: entity_id,
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await?.map_err(ForgeError::from)?;

    // after_delete hook (fire-and-forget)
    if let (Some(dispatcher), Some(snapshot)) = (hook_dispatcher.clone(), pre_delete_snapshot) {
        fire_after_hook(
            AfterHookCtx {
                dispatcher,
                hooks_config: hooks_config.clone(),
                schema: schema_def.clone(),
                event: HookEvent::AfterDelete,
                operation: "delete".to_string(),
                user_id: claims.as_ref().map(|c| c.sub.clone()),
            },
            &snapshot,
        );
    }

    // Audit: entity deleted
    if let Some(logger) = state.audit_logger() {
        logger
            .log_custom(
                "forge.entity.deleted",
                acton_service::audit::AuditSeverity::Warning,
                Some(serde_json::json!({
                    "schema": schema,
                    "entity_id": id,
                    "user": claims.as_ref().map(|c| &c.sub),
                })),
            )
            .await;
    }

    // Webhook: fire notifications
    let webhook_event = crate::webhook::WebhookEvent::from_delete(
        &schema,
        &id,
        claims.as_ref().map(|c| c.sub.as_str()),
    );
    dispatch_webhook(&state, &schema_def, webhook_event, "deleted").await;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Cardinality, FieldDefinition, FieldModifier, FieldName, SchemaId, TextConstraints,
    };

    fn make_test_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("age").unwrap(),
                    FieldType::Integer(
                        schema_forge_core::types::IntegerConstraints::unconstrained(),
                    ),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn json_to_entity_fields_basic() {
        let schema = make_test_schema();
        let mut json_fields = serde_json::Map::new();
        json_fields.insert("name".into(), serde_json::json!("Alice"));
        json_fields.insert("age".into(), serde_json::json!(30));
        json_fields.insert("active".into(), serde_json::json!(true));

        let result = json_to_entity_fields(&schema, &json_fields).unwrap();
        assert_eq!(
            result.get("name"),
            Some(&DynamicValue::Text("Alice".into()))
        );
        assert_eq!(result.get("age"), Some(&DynamicValue::Integer(30)));
        assert_eq!(result.get("active"), Some(&DynamicValue::Boolean(true)));
    }

    #[test]
    fn json_to_entity_fields_missing_required() {
        let schema = make_test_schema();
        let json_fields = serde_json::Map::new(); // missing "name" which is required

        let result = json_to_entity_fields(&schema, &json_fields);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("required") && e.contains("name")));
    }

    #[test]
    fn json_to_entity_fields_type_mismatch() {
        let schema = make_test_schema();
        let mut json_fields = serde_json::Map::new();
        json_fields.insert("name".into(), serde_json::json!("Alice"));
        json_fields.insert("age".into(), serde_json::json!("not a number"));

        let result = json_to_entity_fields(&schema, &json_fields);
        assert!(result.is_err());
    }

    #[test]
    fn json_to_entity_fields_unknown_field_accepted() {
        let schema = make_test_schema();
        let mut json_fields = serde_json::Map::new();
        json_fields.insert("name".into(), serde_json::json!("Alice"));
        json_fields.insert("extra".into(), serde_json::json!("extra value"));

        let result = json_to_entity_fields(&schema, &json_fields).unwrap();
        assert_eq!(
            result.get("extra"),
            Some(&DynamicValue::Text("extra value".into()))
        );
    }

    #[test]
    fn dynamic_value_to_json_primitives() {
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Null),
            serde_json::Value::Null
        );
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Text("hello".into())),
            serde_json::json!("hello")
        );
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Integer(42)),
            serde_json::json!(42)
        );
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Boolean(true)),
            serde_json::json!(true)
        );
    }

    #[test]
    fn entity_to_response_roundtrip() {
        let entity = Entity::new(
            SchemaName::new("Contact").unwrap(),
            BTreeMap::from([
                ("name".to_string(), DynamicValue::Text("Alice".into())),
                ("age".to_string(), DynamicValue::Integer(30)),
            ]),
        );
        let response = entity_to_response(&entity);
        assert_eq!(response.schema, "Contact");
        assert!(response.id.starts_with("entity_"));
        assert_eq!(
            response.fields.get("name"),
            Some(&serde_json::json!("Alice"))
        );
        assert_eq!(response.fields.get("age"), Some(&serde_json::json!(30)));
    }

    #[test]
    fn convert_json_untyped_all_types() {
        assert_eq!(
            convert_json_untyped(&serde_json::json!(null)).unwrap(),
            DynamicValue::Null
        );
        assert_eq!(
            convert_json_untyped(&serde_json::json!(true)).unwrap(),
            DynamicValue::Boolean(true)
        );
        assert_eq!(
            convert_json_untyped(&serde_json::json!(42)).unwrap(),
            DynamicValue::Integer(42)
        );
        assert_eq!(
            convert_json_untyped(&serde_json::json!("text")).unwrap(),
            DynamicValue::Text("text".into())
        );
    }

    #[test]
    fn convert_json_untyped_array() {
        let result = convert_json_untyped(&serde_json::json!([1, 2, 3])).unwrap();
        assert!(matches!(result, DynamicValue::Array(arr) if arr.len() == 3));
    }

    #[test]
    fn convert_json_untyped_object() {
        let result = convert_json_untyped(&serde_json::json!({"key": "value"})).unwrap();
        assert!(matches!(result, DynamicValue::Composite(map) if map.len() == 1));
    }

    #[test]
    fn convert_relation_ref_from_json() {
        let entity_id = EntityId::new();
        let id_str = entity_id.as_str().to_string();
        let field_type = FieldType::Relation {
            target: SchemaName::new("Project").unwrap(),
            cardinality: Cardinality::One,
        };
        let result = convert_json_with_type_hint(&serde_json::json!(id_str), &field_type).unwrap();
        assert!(matches!(result, DynamicValue::Ref(ref id) if id.as_str() == id_str));
    }

    #[test]
    fn convert_relation_ref_null() {
        let field_type = FieldType::Relation {
            target: SchemaName::new("Project").unwrap(),
            cardinality: Cardinality::One,
        };
        let result = convert_json_with_type_hint(&serde_json::json!(null), &field_type).unwrap();
        assert_eq!(result, DynamicValue::Null);
    }

    #[test]
    fn convert_relation_ref_array() {
        let id1 = EntityId::new();
        let id2 = EntityId::new();
        let field_type = FieldType::Relation {
            target: SchemaName::new("Tag").unwrap(),
            cardinality: Cardinality::Many,
        };
        let json = serde_json::json!([id1.as_str(), id2.as_str()]);
        let result = convert_json_with_type_hint(&json, &field_type).unwrap();
        assert!(matches!(result, DynamicValue::RefArray(ids) if ids.len() == 2));
    }

    #[test]
    fn convert_array_with_type_hint() {
        let field_type = FieldType::Array(Box::new(FieldType::Boolean));
        let json = serde_json::json!([true, false, true]);
        let result = convert_json_with_type_hint(&json, &field_type).unwrap();
        assert!(matches!(result, DynamicValue::Array(arr) if arr.len() == 3));
    }

    // -- json_to_filter tests --

    #[test]
    fn json_to_filter_eq() {
        let schema = make_test_schema();
        let json = serde_json::json!({"op": "eq", "field": "name", "value": "Alice"});
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::Eq { .. }));
    }

    #[test]
    fn json_to_filter_gt_integer() {
        let schema = make_test_schema();
        let json = serde_json::json!({"op": "gt", "field": "age", "value": 25});
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(
            filter,
            Filter::Gt { ref value, .. } if *value == DynamicValue::Integer(25)
        ));
    }

    #[test]
    fn json_to_filter_and() {
        let schema = make_test_schema();
        let json = serde_json::json!({
            "op": "and",
            "filters": [
                {"op": "eq", "field": "name", "value": "Alice"},
                {"op": "gt", "field": "age", "value": 25}
            ]
        });
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::And { ref filters } if filters.len() == 2));
    }

    #[test]
    fn json_to_filter_or() {
        let schema = make_test_schema();
        let json = serde_json::json!({
            "op": "or",
            "filters": [
                {"op": "eq", "field": "active", "value": true},
                {"op": "eq", "field": "active", "value": false}
            ]
        });
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::Or { .. }));
    }

    #[test]
    fn json_to_filter_not() {
        let schema = make_test_schema();
        let json = serde_json::json!({
            "op": "not",
            "filter": {"op": "eq", "field": "active", "value": false}
        });
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::Not { .. }));
    }

    #[test]
    fn json_to_filter_contains() {
        let schema = make_test_schema();
        let json = serde_json::json!({"op": "contains", "field": "name", "value": "lic"});
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::Contains { .. }));
    }

    #[test]
    fn json_to_filter_startswith() {
        let schema = make_test_schema();
        let json = serde_json::json!({"op": "startswith", "field": "name", "value": "Al"});
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::StartsWith { .. }));
    }

    #[test]
    fn json_to_filter_in() {
        let schema = make_test_schema();
        let json = serde_json::json!({"op": "in", "field": "age", "values": [20, 25, 30]});
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::In { ref values, .. } if values.len() == 3));
    }

    #[test]
    fn json_to_filter_unknown_op() {
        let schema = make_test_schema();
        let json = serde_json::json!({"op": "regex", "field": "name", "value": ".*"});
        let result = json_to_filter(&json, &schema);
        assert!(result.is_err());
    }

    #[test]
    fn json_to_filter_missing_op() {
        let schema = make_test_schema();
        let json = serde_json::json!({"field": "name", "value": "Alice"});
        let result = json_to_filter(&json, &schema);
        assert!(result.is_err());
    }

    #[test]
    fn json_to_filter_not_object() {
        let schema = make_test_schema();
        let json = serde_json::json!("not an object");
        let result = json_to_filter(&json, &schema);
        assert!(result.is_err());
    }

    #[test]
    fn json_to_filter_nested_and_or() {
        let schema = make_test_schema();
        let json = serde_json::json!({
            "op": "and",
            "filters": [
                {"op": "eq", "field": "name", "value": "Alice"},
                {
                    "op": "or",
                    "filters": [
                        {"op": "gt", "field": "age", "value": 20},
                        {"op": "eq", "field": "active", "value": true}
                    ]
                }
            ]
        });
        let filter = json_to_filter(&json, &schema).unwrap();
        assert!(matches!(filter, Filter::And { ref filters } if filters.len() == 2));
    }

    // -- EntityQueryBody fields deserialization --

    #[test]
    fn query_body_deserializes_fields() {
        let json = serde_json::json!({
            "fields": ["name", "age"],
            "limit": 10
        });
        let body: EntityQueryBody = serde_json::from_value(json).unwrap();
        assert_eq!(
            body.fields,
            Some(vec!["name".to_string(), "age".to_string()])
        );
        assert_eq!(body.limit, Some(10));
    }

    #[test]
    fn query_body_fields_defaults_to_none() {
        let json = serde_json::json!({ "limit": 10 });
        let body: EntityQueryBody = serde_json::from_value(json).unwrap();
        assert!(body.fields.is_none());
    }

    // -- Projection applied to entity fields --

    #[test]
    fn projection_filters_entity_fields() {
        let mut entity = Entity::new(
            SchemaName::new("Contact").unwrap(),
            BTreeMap::from([
                ("name".to_string(), DynamicValue::Text("Alice".into())),
                ("age".to_string(), DynamicValue::Integer(30)),
                ("active".to_string(), DynamicValue::Boolean(true)),
            ]),
        );
        let proj: HashSet<String> = ["name".to_string()].into_iter().collect();
        entity.fields.retain(|k, _| proj.contains(k));
        let response = entity_to_response(&entity);
        assert_eq!(response.fields.len(), 1);
        assert_eq!(
            response.fields.get("name"),
            Some(&serde_json::json!("Alice"))
        );
        assert!(response.fields.get("age").is_none());
        assert!(response.fields.get("active").is_none());
    }

    #[test]
    fn projection_none_returns_all_fields() {
        let entity = Entity::new(
            SchemaName::new("Contact").unwrap(),
            BTreeMap::from([
                ("name".to_string(), DynamicValue::Text("Alice".into())),
                ("age".to_string(), DynamicValue::Integer(30)),
            ]),
        );
        let response = entity_to_response(&entity);
        assert_eq!(response.fields.len(), 2);
    }

    // -----------------------------------------------------------------
    // coerce_dynamic_value_with_type_hint (issue #6 regression coverage)
    // -----------------------------------------------------------------

    #[test]
    fn coerce_datetime_from_text_succeeds() {
        let result = coerce_dynamic_value_with_type_hint(
            DynamicValue::Text("2025-04-12T10:00:00Z".into()),
            &FieldType::DateTime,
        )
        .unwrap();
        let expected = "2025-04-12T10:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        assert_eq!(result, DynamicValue::DateTime(expected));
    }

    #[test]
    fn coerce_datetime_from_text_invalid_returns_err() {
        let err = coerce_dynamic_value_with_type_hint(
            DynamicValue::Text("not-a-date".into()),
            &FieldType::DateTime,
        )
        .unwrap_err();
        assert!(err.contains("invalid datetime"), "unexpected error: {err}");
        assert!(err.contains("not-a-date"));
    }

    #[test]
    fn coerce_datetime_passthrough() {
        let dt = chrono::Utc::now();
        let result =
            coerce_dynamic_value_with_type_hint(DynamicValue::DateTime(dt), &FieldType::DateTime)
                .unwrap();
        assert_eq!(result, DynamicValue::DateTime(dt));
    }

    #[test]
    fn coerce_datetime_null_passthrough() {
        let result =
            coerce_dynamic_value_with_type_hint(DynamicValue::Null, &FieldType::DateTime).unwrap();
        assert_eq!(result, DynamicValue::Null);
    }

    #[test]
    fn coerce_enum_from_text() {
        let enum_type = FieldType::Enum(
            schema_forge_core::types::EnumVariants::new(vec!["Active".into(), "Inactive".into()])
                .unwrap(),
        );
        let result =
            coerce_dynamic_value_with_type_hint(DynamicValue::Text("Active".into()), &enum_type)
                .unwrap();
        assert_eq!(result, DynamicValue::Enum("Active".into()));
    }

    #[test]
    fn coerce_relation_from_text_parses_entity_id() {
        let relation = FieldType::Relation {
            target: SchemaName::new("Company").unwrap(),
            cardinality: Cardinality::One,
        };
        let id = EntityId::new();
        let result = coerce_dynamic_value_with_type_hint(
            DynamicValue::Text(id.as_str().to_string()),
            &relation,
        )
        .unwrap();
        assert_eq!(result, DynamicValue::Ref(id));
    }

    #[test]
    fn coerce_relation_from_text_invalid_returns_err() {
        let relation = FieldType::Relation {
            target: SchemaName::new("Company").unwrap(),
            cardinality: Cardinality::One,
        };
        let err =
            coerce_dynamic_value_with_type_hint(DynamicValue::Text("not-an-id".into()), &relation)
                .unwrap_err();
        assert!(
            err.contains("invalid entity reference"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn coerce_relation_many_from_array_of_text() {
        let relation = FieldType::Relation {
            target: SchemaName::new("Tag").unwrap(),
            cardinality: Cardinality::Many,
        };
        let id1 = EntityId::new();
        let id2 = EntityId::new();
        let result = coerce_dynamic_value_with_type_hint(
            DynamicValue::Array(vec![
                DynamicValue::Text(id1.as_str().to_string()),
                DynamicValue::Text(id2.as_str().to_string()),
            ]),
            &relation,
        )
        .unwrap();
        assert_eq!(result, DynamicValue::RefArray(vec![id1, id2]));
    }

    #[test]
    fn coerce_array_of_datetime_recurses() {
        let array_type = FieldType::Array(Box::new(FieldType::DateTime));
        let result = coerce_dynamic_value_with_type_hint(
            DynamicValue::Array(vec![
                DynamicValue::Text("2025-04-12T10:00:00Z".into()),
                DynamicValue::Text("2025-04-13T10:00:00Z".into()),
            ]),
            &array_type,
        )
        .unwrap();
        match result {
            DynamicValue::Array(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0], DynamicValue::DateTime(_)));
                assert!(matches!(items[1], DynamicValue::DateTime(_)));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn coerce_text_passthrough() {
        let result = coerce_dynamic_value_with_type_hint(
            DynamicValue::Text("hello".into()),
            &FieldType::Text(TextConstraints::unconstrained()),
        )
        .unwrap();
        assert_eq!(result, DynamicValue::Text("hello".into()));
    }

    #[test]
    fn coerce_integer_from_text() {
        let result = coerce_dynamic_value_with_type_hint(
            DynamicValue::Text("42".into()),
            &FieldType::Integer(schema_forge_core::types::IntegerConstraints::unconstrained()),
        )
        .unwrap();
        assert_eq!(result, DynamicValue::Integer(42));
    }
}
