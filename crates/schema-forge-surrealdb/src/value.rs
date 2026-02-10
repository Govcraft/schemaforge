//! Pure functions for converting between `DynamicValue` and `surrealdb::sql::Value`.
//!
//! These conversions are used when reading from and writing to SurrealDB.
//!
//! We use the `surrealdb::sql` module types (re-exported from `surrealdb_core`)
//! for pattern matching on query results. Construction of composite values
//! goes through the public `surrealdb::Object` wrapper which exposes `insert`.

use std::collections::BTreeMap;

use schema_forge_backend::entity::Entity;
use schema_forge_backend::error::BackendError;
use schema_forge_core::types::{DynamicValue, EntityId, SchemaName};
use surrealdb::sql::Value as SurrealValue;

/// Convert a `DynamicValue` to a `surrealdb::sql::Value`.
pub fn dynamic_to_surreal(value: &DynamicValue) -> SurrealValue {
    match value {
        DynamicValue::Null => SurrealValue::None,
        DynamicValue::Text(s) => SurrealValue::from(s.as_str()),
        DynamicValue::Integer(i) => SurrealValue::from(*i),
        DynamicValue::Float(f) => SurrealValue::from(*f),
        DynamicValue::Boolean(b) => SurrealValue::from(*b),
        DynamicValue::DateTime(dt) => {
            // Store as ISO 8601 string -- SurrealDB will accept this for datetime fields.
            SurrealValue::from(dt.to_rfc3339())
        }
        DynamicValue::Enum(s) => SurrealValue::from(s.as_str()),
        DynamicValue::Json(v) => json_to_surreal(v),
        DynamicValue::Array(arr) => {
            let items: Vec<SurrealValue> = arr.iter().map(dynamic_to_surreal).collect();
            SurrealValue::from(items)
        }
        DynamicValue::Composite(map) => {
            let mut obj = surrealdb::Object::new();
            for (k, v) in map {
                obj.insert(k.clone(), surrealdb::Value::from_inner(dynamic_to_surreal(v)));
            }
            SurrealValue::Object(obj.into_inner())
        }
        DynamicValue::Ref(id) => SurrealValue::from(id.as_str()),
        DynamicValue::RefArray(ids) => {
            let items: Vec<SurrealValue> =
                ids.iter().map(|id| SurrealValue::from(id.as_str())).collect();
            SurrealValue::from(items)
        }
        _ => {
            // Future DynamicValue variants -- store as string fallback.
            SurrealValue::from(format!("{value:?}").as_str())
        }
    }
}

/// Convert a `surrealdb::sql::Value` back to a `DynamicValue`.
///
/// This is a best-effort conversion. SurrealDB values that do not have
/// a corresponding `DynamicValue` variant are stored as JSON.
pub fn surreal_to_dynamic(value: &SurrealValue) -> Result<DynamicValue, BackendError> {
    match value {
        SurrealValue::None | SurrealValue::Null => Ok(DynamicValue::Null),
        SurrealValue::Bool(b) => Ok(DynamicValue::Boolean(*b)),
        SurrealValue::Number(n) => {
            // Match on the Number enum variants directly.
            match n {
                surrealdb::sql::Number::Int(i) => Ok(DynamicValue::Integer(*i)),
                surrealdb::sql::Number::Float(f) => Ok(DynamicValue::Float(*f)),
                _ => {
                    // Decimal or future variants -- convert to float.
                    Ok(DynamicValue::Float((*n).as_float()))
                }
            }
        }
        SurrealValue::Strand(s) => Ok(DynamicValue::Text(s.0.clone())),
        SurrealValue::Datetime(dt) => {
            // surrealdb_core::sql::Datetime wraps chrono::DateTime<Utc> as pub field .0
            let chrono_dt: chrono::DateTime<chrono::Utc> = dt.0;
            Ok(DynamicValue::DateTime(chrono_dt))
        }
        SurrealValue::Array(arr) => {
            let items: Result<Vec<DynamicValue>, BackendError> =
                arr.iter().map(surreal_to_dynamic).collect();
            Ok(DynamicValue::Array(items?))
        }
        SurrealValue::Object(obj) => {
            let mut map = BTreeMap::new();
            for (k, v) in obj.iter() {
                map.insert(k.clone(), surreal_to_dynamic(v)?);
            }
            Ok(DynamicValue::Composite(map))
        }
        _ => {
            // Fallback: convert to JSON representation
            let json_str = value.to_string();
            match serde_json::from_str::<serde_json::Value>(&json_str) {
                Ok(json_val) => Ok(DynamicValue::Json(json_val)),
                Err(_) => Ok(DynamicValue::Text(json_str)),
            }
        }
    }
}

/// Convert an `Entity` to a `BTreeMap` of SurrealDB values for insertion.
///
/// The entity ID is stored under the `"id"` key as a plain string.
pub fn entity_to_surreal_map(entity: &Entity) -> BTreeMap<String, SurrealValue> {
    let mut map = BTreeMap::new();
    map.insert("id".to_string(), SurrealValue::from(entity.id.as_str()));
    for (k, v) in &entity.fields {
        map.insert(k.clone(), dynamic_to_surreal(v));
    }
    map
}

/// Convert a SurrealDB object (query result row) back to an `Entity`.
///
/// Expects an `"id"` field containing the entity's identifier.
pub fn surreal_object_to_entity(
    schema: &SchemaName,
    obj: &surrealdb::sql::Object,
) -> Result<Entity, BackendError> {
    // Extract ID
    let id_value = obj.get("id").ok_or_else(|| BackendError::Internal {
        message: "SurrealDB record missing 'id' field".to_string(),
    })?;

    let id_str = extract_id_string(id_value)?;
    let entity_id = EntityId::parse(&id_str).map_err(|e| BackendError::Internal {
        message: format!("failed to parse entity ID '{id_str}': {e}"),
    })?;

    // Convert remaining fields
    let mut fields = BTreeMap::new();
    for (k, v) in obj.iter() {
        if k == "id" {
            continue;
        }
        fields.insert(k.clone(), surreal_to_dynamic(v)?);
    }

    Ok(Entity::with_id(entity_id, schema.clone(), fields))
}

/// Extract a string representation of an ID from a SurrealDB value.
///
/// SurrealDB may return IDs as `Thing` (table:id), `Strand`, or other formats.
fn extract_id_string(value: &SurrealValue) -> Result<String, BackendError> {
    match value {
        SurrealValue::Strand(s) => Ok(s.0.clone()),
        SurrealValue::Thing(thing) => {
            // thing.id is the record's unique part; thing.tb is the table name
            Ok(thing.id.to_raw())
        }
        other => Ok(other.to_string()),
    }
}

/// Convert a `serde_json::Value` to a `surrealdb::sql::Value`.
fn json_to_surreal(json: &serde_json::Value) -> SurrealValue {
    match json {
        serde_json::Value::Null => SurrealValue::None,
        serde_json::Value::Bool(b) => SurrealValue::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SurrealValue::from(i)
            } else if let Some(f) = n.as_f64() {
                SurrealValue::from(f)
            } else {
                SurrealValue::from(n.to_string().as_str())
            }
        }
        serde_json::Value::String(s) => SurrealValue::from(s.as_str()),
        serde_json::Value::Array(arr) => {
            let items: Vec<SurrealValue> = arr.iter().map(json_to_surreal).collect();
            SurrealValue::from(items)
        }
        serde_json::Value::Object(map) => {
            let mut obj = surrealdb::Object::new();
            for (k, v) in map {
                obj.insert(k.clone(), surrealdb::Value::from_inner(json_to_surreal(v)));
            }
            SurrealValue::Object(obj.into_inner())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_round_trip() {
        let dv = DynamicValue::Null;
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, DynamicValue::Null);
    }

    #[test]
    fn text_round_trip() {
        let dv = DynamicValue::Text("hello".into());
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, DynamicValue::Text("hello".into()));
    }

    #[test]
    fn integer_round_trip() {
        let dv = DynamicValue::Integer(42);
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, DynamicValue::Integer(42));
    }

    #[test]
    fn float_round_trip() {
        let dv = DynamicValue::Float(3.14);
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        // Float comparison -- check it came back as Float
        match back {
            DynamicValue::Float(f) => assert!((f - 3.14).abs() < f64::EPSILON),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn boolean_round_trip() {
        let dv = DynamicValue::Boolean(true);
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, DynamicValue::Boolean(true));
    }

    #[test]
    fn array_round_trip() {
        let dv = DynamicValue::Array(vec![
            DynamicValue::Integer(1),
            DynamicValue::Integer(2),
        ]);
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(
            back,
            DynamicValue::Array(vec![
                DynamicValue::Integer(1),
                DynamicValue::Integer(2),
            ])
        );
    }

    #[test]
    fn composite_round_trip() {
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), DynamicValue::Text("value".into()));
        let dv = DynamicValue::Composite(map.clone());
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, DynamicValue::Composite(map));
    }

    #[test]
    fn enum_converts_to_text() {
        let dv = DynamicValue::Enum("Active".into());
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        // Enum maps to string in SurrealDB, comes back as Text
        assert_eq!(back, DynamicValue::Text("Active".into()));
    }
}
