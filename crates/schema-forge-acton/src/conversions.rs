use schema_forge_backend::entity::Entity;
use schema_forge_core::types::DynamicValue;

use crate::routes::entities::EntityResponse;

/// Convert a `DynamicValue` to a JSON value.
pub fn dynamic_value_to_json(value: &DynamicValue) -> serde_json::Value {
    match value {
        DynamicValue::Null => serde_json::Value::Null,
        DynamicValue::Text(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Integer(i) => serde_json::json!(i),
        DynamicValue::Float(f) => serde_json::json!(f),
        DynamicValue::Boolean(b) => serde_json::Value::Bool(*b),
        DynamicValue::DateTime(dt) => serde_json::Value::String(dt.to_rfc3339()),
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

/// Convert an `Entity` to an `EntityResponse`.
pub fn entity_to_response(entity: &Entity) -> EntityResponse {
    let mut fields = serde_json::Map::new();
    for (key, value) in &entity.fields {
        fields.insert(key.clone(), dynamic_value_to_json(value));
    }

    EntityResponse {
        id: entity.id.as_str().to_string(),
        schema: entity.schema.as_str().to_string(),
        fields,
    }
}
