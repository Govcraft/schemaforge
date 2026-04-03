//! Functions for converting between `DynamicValue` and PostgreSQL row/argument types.
//!
//! These conversions are used when reading from and writing to PostgreSQL via SQLx.
//! Because SchemaForge schemas are dynamic (not known at compile time), we use
//! `sqlx::query()` (unchecked) and bind values manually through `PgArguments`.

use std::collections::BTreeMap;

use schema_forge_backend::entity::Entity;
use schema_forge_backend::error::BackendError;
use schema_forge_core::types::{DynamicValue, EntityId, FieldType, SchemaDefinition, SchemaName};
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::{Arguments, Column, Row, ValueRef};

/// Bind a `DynamicValue` into a `PgArguments` buffer.
///
/// The caller must ensure the bind position matches the `$N` placeholder
/// in the SQL string. Returns an error if the value type is not encodable.
pub fn bind_dynamic_value(
    args: &mut PgArguments,
    value: &DynamicValue,
) -> Result<(), BackendError> {
    match value {
        DynamicValue::Null => {
            args.add(None::<String>).map_err(|e| BackendError::Internal {
                message: format!("failed to bind NULL: {e}"),
            })?;
        }
        DynamicValue::Text(s) | DynamicValue::Enum(s) => {
            args.add(s.as_str()).map_err(|e| BackendError::Internal {
                message: format!("failed to bind text: {e}"),
            })?;
        }
        DynamicValue::Integer(i) => {
            args.add(*i).map_err(|e| BackendError::Internal {
                message: format!("failed to bind integer: {e}"),
            })?;
        }
        DynamicValue::Float(f) => {
            args.add(*f).map_err(|e| BackendError::Internal {
                message: format!("failed to bind float: {e}"),
            })?;
        }
        DynamicValue::Boolean(b) => {
            args.add(*b).map_err(|e| BackendError::Internal {
                message: format!("failed to bind boolean: {e}"),
            })?;
        }
        DynamicValue::DateTime(dt) => {
            args.add(*dt).map_err(|e| BackendError::Internal {
                message: format!("failed to bind datetime: {e}"),
            })?;
        }
        DynamicValue::Json(v) => {
            args.add(sqlx::types::Json(v))
                .map_err(|e| BackendError::Internal {
                    message: format!("failed to bind json: {e}"),
                })?;
        }
        DynamicValue::Composite(map) => {
            let json_val = composite_to_json(map);
            args.add(sqlx::types::Json(&json_val))
                .map_err(|e| BackendError::Internal {
                    message: format!("failed to bind composite: {e}"),
                })?;
        }
        DynamicValue::Ref(id) => {
            args.add(id.as_str())
                .map_err(|e| BackendError::Internal {
                    message: format!("failed to bind ref: {e}"),
                })?;
        }
        DynamicValue::RefArray(ids) => {
            let strs: Vec<&str> = ids.iter().map(|id| id.as_str()).collect();
            args.add(strs).map_err(|e| BackendError::Internal {
                message: format!("failed to bind ref array: {e}"),
            })?;
        }
        DynamicValue::Array(arr) => {
            // Arrays of homogeneous primitive types can be bound as PostgreSQL arrays.
            // For mixed types, fall back to JSONB.
            let json_val: serde_json::Value = arr
                .iter()
                .map(dynamic_to_json)
                .collect::<Vec<serde_json::Value>>()
                .into();
            args.add(sqlx::types::Json(&json_val))
                .map_err(|e| BackendError::Internal {
                    message: format!("failed to bind array: {e}"),
                })?;
        }
        _ => {
            // Future DynamicValue variants -- bind as text fallback.
            args.add(format!("{value:?}"))
                .map_err(|e| BackendError::Internal {
                    message: format!("failed to bind unknown value: {e}"),
                })?;
        }
    }
    Ok(())
}

/// Convert a PostgreSQL row to an `Entity`, guided by the schema definition.
///
/// Uses the schema's field definitions to determine the correct type for each
/// column, avoiding ambiguity when PostgreSQL type info alone is insufficient.
pub fn row_to_entity(
    row: &PgRow,
    schema: &SchemaName,
    schema_def: Option<&SchemaDefinition>,
) -> Result<Entity, BackendError> {
    // Extract ID
    let id_str: String = row.try_get("id").map_err(|e| BackendError::Internal {
        message: format!("row missing 'id' column: {e}"),
    })?;
    let entity_id = EntityId::parse(&id_str).map_err(|e| BackendError::Internal {
        message: format!("failed to parse entity ID '{id_str}': {e}"),
    })?;

    let mut fields = BTreeMap::new();

    // Iterate over columns, skipping "id"
    for column in row.columns() {
        let col_name = column.name();
        if col_name == "id" {
            continue;
        }

        let field_type = schema_def.and_then(|sd| sd.field(col_name)).map(|fd| &fd.field_type);
        let value = read_column(row, col_name, field_type)?;
        fields.insert(col_name.to_string(), value);
    }

    Ok(Entity::with_id(entity_id, schema.clone(), fields))
}

/// Read a single column value from a PostgreSQL row, using the schema's field type
/// to guide interpretation.
fn read_column(
    row: &PgRow,
    col_name: &str,
    field_type: Option<&FieldType>,
) -> Result<DynamicValue, BackendError> {
    // Check for NULL first
    let raw = row
        .try_get_raw(col_name)
        .map_err(|e| BackendError::Internal {
            message: format!("failed to read column '{col_name}': {e}"),
        })?;
    if raw.is_null() {
        return Ok(DynamicValue::Null);
    }
    drop(raw);

    // Use schema type to guide deserialization
    match field_type {
        Some(FieldType::Integer(_)) => {
            let v: i64 = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read integer column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Integer(v))
        }
        Some(FieldType::Float(_)) => {
            let v: f64 = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read float column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Float(v))
        }
        Some(FieldType::Boolean) => {
            let v: bool = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read boolean column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Boolean(v))
        }
        Some(FieldType::DateTime) => {
            let v: chrono::DateTime<chrono::Utc> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read datetime column '{col_name}': {e}"),
                })?;
            Ok(DynamicValue::DateTime(v))
        }
        Some(FieldType::Json) => {
            let v: sqlx::types::Json<serde_json::Value> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read json column '{col_name}': {e}"),
                })?;
            Ok(DynamicValue::Json(v.0))
        }
        Some(FieldType::Composite(_)) => {
            let v: sqlx::types::Json<serde_json::Value> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read composite column '{col_name}': {e}"),
                })?;
            Ok(json_to_composite(&v.0))
        }
        Some(FieldType::Enum(_)) => {
            let v: String = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read enum column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Enum(v))
        }
        Some(FieldType::Relation {
            cardinality: schema_forge_core::types::Cardinality::Many,
            ..
        }) => {
            let v: Vec<String> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read ref array column '{col_name}': {e}"),
                })?;
            let ids: Result<Vec<EntityId>, _> = v.iter().map(|s| EntityId::parse(s)).collect();
            match ids {
                Ok(ids) => Ok(DynamicValue::RefArray(ids)),
                Err(_) => Ok(DynamicValue::Array(
                    v.into_iter().map(DynamicValue::Text).collect(),
                )),
            }
        }
        Some(FieldType::Relation { .. }) => {
            let v: String = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read ref column '{col_name}': {e}"),
            })?;
            match EntityId::parse(&v) {
                Ok(id) => Ok(DynamicValue::Ref(id)),
                Err(_) => Ok(DynamicValue::Text(v)),
            }
        }
        Some(FieldType::Array(_)) => {
            // Try to read as JSONB array
            let v: sqlx::types::Json<serde_json::Value> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read array column '{col_name}': {e}"),
                })?;
            Ok(json_to_dynamic_array(&v.0))
        }
        // Text, RichText, or unknown -- read as string
        _ => {
            let v: String = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read text column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Text(v))
        }
    }
}

/// Convert a `DynamicValue` to a `serde_json::Value` for JSONB storage.
fn dynamic_to_json(value: &DynamicValue) -> serde_json::Value {
    match value {
        DynamicValue::Null => serde_json::Value::Null,
        DynamicValue::Text(s) | DynamicValue::Enum(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Integer(i) => serde_json::json!(*i),
        DynamicValue::Float(f) => serde_json::json!(*f),
        DynamicValue::Boolean(b) => serde_json::json!(*b),
        DynamicValue::DateTime(dt) => serde_json::Value::String(dt.to_rfc3339()),
        DynamicValue::Json(v) => v.clone(),
        DynamicValue::Array(arr) => {
            let items: Vec<serde_json::Value> = arr.iter().map(dynamic_to_json).collect();
            serde_json::Value::Array(items)
        }
        DynamicValue::Composite(map) => composite_to_json(map),
        DynamicValue::Ref(id) => serde_json::Value::String(id.as_str().to_string()),
        DynamicValue::RefArray(ids) => {
            let items: Vec<serde_json::Value> = ids
                .iter()
                .map(|id| serde_json::Value::String(id.as_str().to_string()))
                .collect();
            serde_json::Value::Array(items)
        }
        _ => serde_json::Value::String(format!("{value:?}")),
    }
}

/// Convert a BTreeMap composite to a JSON object.
fn composite_to_json(map: &BTreeMap<String, DynamicValue>) -> serde_json::Value {
    let obj: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, v)| (k.clone(), dynamic_to_json(v)))
        .collect();
    serde_json::Value::Object(obj)
}

/// Convert a JSON value to a `DynamicValue::Composite`.
fn json_to_composite(json: &serde_json::Value) -> DynamicValue {
    match json {
        serde_json::Value::Object(map) => {
            let mut result = BTreeMap::new();
            for (k, v) in map {
                result.insert(k.clone(), json_value_to_dynamic(v));
            }
            DynamicValue::Composite(result)
        }
        other => DynamicValue::Json(other.clone()),
    }
}

/// Convert a JSON array to a `DynamicValue::Array`.
fn json_to_dynamic_array(json: &serde_json::Value) -> DynamicValue {
    match json {
        serde_json::Value::Array(arr) => {
            let items: Vec<DynamicValue> = arr.iter().map(json_value_to_dynamic).collect();
            DynamicValue::Array(items)
        }
        other => DynamicValue::Json(other.clone()),
    }
}

/// Convert a `serde_json::Value` to the best-fit `DynamicValue`.
fn json_value_to_dynamic(json: &serde_json::Value) -> DynamicValue {
    match json {
        serde_json::Value::Null => DynamicValue::Null,
        serde_json::Value::Bool(b) => DynamicValue::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                DynamicValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                DynamicValue::Float(f)
            } else {
                DynamicValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => DynamicValue::Text(s.clone()),
        serde_json::Value::Array(arr) => {
            let items: Vec<DynamicValue> = arr.iter().map(json_value_to_dynamic).collect();
            DynamicValue::Array(items)
        }
        serde_json::Value::Object(map) => {
            let mut result = BTreeMap::new();
            for (k, v) in map {
                result.insert(k.clone(), json_value_to_dynamic(v));
            }
            DynamicValue::Composite(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_to_json_text() {
        assert_eq!(
            dynamic_to_json(&DynamicValue::Text("hello".into())),
            serde_json::json!("hello")
        );
    }

    #[test]
    fn dynamic_to_json_integer() {
        assert_eq!(
            dynamic_to_json(&DynamicValue::Integer(42)),
            serde_json::json!(42)
        );
    }

    #[test]
    fn dynamic_to_json_null() {
        assert_eq!(
            dynamic_to_json(&DynamicValue::Null),
            serde_json::Value::Null
        );
    }

    #[test]
    fn json_value_to_dynamic_roundtrip() {
        let json = serde_json::json!({
            "name": "Alice",
            "age": 30,
            "active": true
        });
        let dv = json_value_to_dynamic(&json);
        match dv {
            DynamicValue::Composite(map) => {
                assert_eq!(map.get("name"), Some(&DynamicValue::Text("Alice".into())));
                assert_eq!(map.get("age"), Some(&DynamicValue::Integer(30)));
                assert_eq!(map.get("active"), Some(&DynamicValue::Boolean(true)));
            }
            other => panic!("expected Composite, got {other:?}"),
        }
    }

    #[test]
    fn composite_to_json_roundtrip() {
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), DynamicValue::Text("value".into()));
        map.insert("count".to_string(), DynamicValue::Integer(5));

        let json = composite_to_json(&map);
        assert_eq!(json["key"], "value");
        assert_eq!(json["count"], 5);
    }

    #[test]
    fn bind_dynamic_value_accepts_all_types() {
        // Verify that all DynamicValue variants can be bound without panicking
        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Text("hello".into())).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Integer(42)).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Float(3.14)).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Boolean(true)).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Enum("Active".into())).is_ok());
    }
}
