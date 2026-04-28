use chrono::SecondsFormat;
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{DynamicValue, SchemaDefinition};

use crate::routes::entities::EntityResponse;

/// Convert a `DynamicValue` to a JSON value.
///
/// Datetime values are emitted in RFC 3339 form with a `Z` UTC marker
/// (e.g. `2026-04-13T12:34:56.789Z`). Using the `Z` suffix ensures that
/// `GET → modify → PUT` round-trips parse back via
/// `chrono::DateTime::<Utc>::from_str`, which rejects `+00:00` offsets in
/// some downstream clients but always accepts `Z`.
pub fn dynamic_value_to_json(value: &DynamicValue) -> serde_json::Value {
    match value {
        DynamicValue::Null => serde_json::Value::Null,
        DynamicValue::Text(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Integer(i) => serde_json::json!(i),
        DynamicValue::Float(f) => serde_json::json!(f),
        DynamicValue::Boolean(b) => serde_json::Value::Bool(*b),
        DynamicValue::DateTime(dt) => {
            serde_json::Value::String(dt.to_rfc3339_opts(SecondsFormat::Millis, true))
        }
        DynamicValue::Enum(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Json(v) => v.clone(),
        DynamicValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(dynamic_value_to_json).collect())
        }
        DynamicValue::Composite(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), dynamic_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        DynamicValue::Ref(id) => serde_json::Value::String(id.as_str().to_string()),
        DynamicValue::RefArray(ids) => serde_json::Value::Array(
            ids.iter()
                .map(|id| serde_json::Value::String(id.as_str().to_string()))
                .collect(),
        ),
        _ => serde_json::Value::Null,
    }
}

/// Convert an `Entity` to an `EntityResponse`, omitting any field that
/// the schema marks `@hidden`.
///
/// `@hidden` is the language-level contract for "never let this leave
/// the storage layer" — it covers `password_hash` on the system `User`
/// schema and any other field operators add the annotation to. Stripping
/// here means every API surface that runs through `entity_to_response`
/// (REST, GraphQL list/get/query/create/update/patch) is automatically
/// safe; consumers that need the raw value must read the entity
/// out-of-band.
pub fn entity_to_response(entity: &Entity, schema: &SchemaDefinition) -> EntityResponse {
    let mut fields = serde_json::Map::new();
    for (key, value) in &entity.fields {
        if let Some(field_def) = schema.field(key) {
            if field_def.is_hidden() {
                continue;
            }
        }
        fields.insert(key.clone(), dynamic_value_to_json(value));
    }

    EntityResponse {
        id: entity.id.as_str().to_string(),
        schema: entity.schema.as_str().to_string(),
        fields,
        permissions: None,
    }
}
