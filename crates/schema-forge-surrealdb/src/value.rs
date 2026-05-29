//! Pure functions for converting between `DynamicValue` and `surrealdb::sql::Value`.
//!
//! These conversions are used when reading from and writing to SurrealDB.
//!
//! We use the `surrealdb::sql` module types (re-exported from `surrealdb_core`)
//! for pattern matching on query results. Construction of composite values
//! goes through the public `surrealdb::Object` wrapper which exposes `insert`.
//!
//! A SchemaForge `duration` is a signed [`chrono::TimeDelta`], but SurrealDB's
//! native `duration` type is unsigned. A negative duration therefore cannot be
//! stored faithfully and is REJECTED fail-closed on write (see
//! [`first_negative_duration`]); it is never silently coerced to NULL.

use std::collections::BTreeMap;

use schema_forge_backend::entity::Entity;
use schema_forge_backend::error::BackendError;
use schema_forge_core::types::{DynamicValue, EntityId, FieldType, SchemaName};
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
            // Store as ISO 8601 string — the literal serializer in backend.rs
            // will wrap it with d'...' for SurrealQL datetime fields.
            SurrealValue::from(dt.to_rfc3339())
        }
        DynamicValue::Duration(d) => {
            timedelta_to_surreal_duration(d).map_or(SurrealValue::None, SurrealValue::Duration)
        }
        DynamicValue::Bytes(b) => {
            // SurrealDB has a native (unsigned-length) `bytes` type; store the
            // bytes verbatim.
            SurrealValue::Bytes(surrealdb::sql::Bytes::from(b.clone()))
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
                obj.insert(
                    k.clone(),
                    surrealdb::Value::from_inner(dynamic_to_surreal(v)),
                );
            }
            SurrealValue::Object(obj.into_inner())
        }
        DynamicValue::Ref(id) => SurrealValue::from(id.as_str()),
        DynamicValue::RefArray(ids) => {
            let items: Vec<SurrealValue> = ids
                .iter()
                .map(|id| SurrealValue::from(id.as_str()))
                .collect();
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
        SurrealValue::Duration(dur) => {
            // surrealdb::sql::Duration wraps an unsigned std::time::Duration.
            let delta = chrono::TimeDelta::from_std(dur.0).map_err(|e| BackendError::Internal {
                message: format!("duration out of representable range: {e}"),
            })?;
            Ok(DynamicValue::Duration(delta))
        }
        SurrealValue::Bytes(b) => Ok(DynamicValue::Bytes(b.to_vec())),
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
        SurrealValue::Thing(thing) => {
            // Record reference from a relation field
            let id_str = thing.id.to_raw();
            match EntityId::parse(&id_str) {
                Ok(entity_id) => Ok(DynamicValue::Ref(entity_id)),
                Err(_) => Ok(DynamicValue::Text(format!("{}:{}", thing.tb, id_str))),
            }
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

/// Convert a signed `chrono::TimeDelta` to SurrealDB's native (unsigned)
/// `surrealdb::sql::Duration`.
///
/// SurrealDB durations wrap an unsigned `std::time::Duration`, so a negative
/// `TimeDelta` has no native representation and yields `None`. A negative value
/// must NEVER be silently coerced to NULL on write — the write path
/// ([`crate::backend`]) rejects it fail-closed via
/// [`first_negative_duration`] before this conversion is reached. Practical
/// `duration` field uses on a records platform (retention windows, TTLs, SLA
/// timers) are non-negative, so `None` here is only ever the unreachable
/// belt-and-braces case for an already-validated value.
fn timedelta_to_surreal_duration(d: &chrono::TimeDelta) -> Option<surrealdb::sql::Duration> {
    d.to_std().ok().map(surrealdb::sql::Duration::from)
}

/// Find the first negative `duration` anywhere in a value tree.
///
/// SurrealDB's native `duration` type is unsigned, so a negative
/// [`chrono::TimeDelta`] cannot be stored faithfully. The write path uses this
/// to reject such a value with a clear error rather than silently dropping it
/// to NULL. Recurses through arrays and composites so a negative duration
/// nested inside a `duration[]` field or a composite is also caught.
///
/// Returns the offending [`chrono::TimeDelta`] (the first encountered) or
/// `None` when every duration in the tree is non-negative.
pub(crate) fn first_negative_duration(value: &DynamicValue) -> Option<chrono::TimeDelta> {
    match value {
        DynamicValue::Duration(d) if *d < chrono::TimeDelta::zero() => Some(*d),
        DynamicValue::Array(items) => items.iter().find_map(first_negative_duration),
        DynamicValue::Composite(map) => map.values().find_map(first_negative_duration),
        _ => None,
    }
}

/// Find the first oversized `bytes` value relative to its declared `max_size`,
/// walking arrays and composites in lock-step with the field type.
///
/// The byte-length cap on a `bytes` field must be enforced fail-closed on write:
/// an oversized value is rejected with a clear error, never silently stored.
/// Recurses through `Array<bytes>` and `Composite` so a nested oversized value
/// is also caught.
///
/// Returns `(actual_len, max_size)` for the first violation, or `None` when no
/// `bytes` value in the tree exceeds its cap (or no cap is set).
pub(crate) fn first_oversized_bytes(
    field_type: &FieldType,
    value: &DynamicValue,
) -> Option<(usize, usize)> {
    match (field_type, value) {
        (FieldType::Bytes(constraints), DynamicValue::Bytes(b)) => {
            let max = constraints.max_size?;
            (b.len() > max).then_some((b.len(), max))
        }
        (FieldType::Array(inner), DynamicValue::Array(items)) => items
            .iter()
            .find_map(|item| first_oversized_bytes(inner, item)),
        (FieldType::Composite(fields), DynamicValue::Composite(map)) => {
            fields.iter().find_map(|fd| {
                map.get(fd.name.as_str())
                    .and_then(|v| first_oversized_bytes(&fd.field_type, v))
            })
        }
        _ => None,
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
        let dv = DynamicValue::Float(42.5);
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        // Float comparison -- check it came back as Float
        match back {
            DynamicValue::Float(f) => assert!((f - 42.5).abs() < f64::EPSILON),
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
    fn duration_round_trip_positive() {
        let dv = DynamicValue::Duration(chrono::TimeDelta::seconds(220_752_000));
        let sv = dynamic_to_surreal(&dv);
        assert!(matches!(sv, SurrealValue::Duration(_)));
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, dv);
    }

    #[test]
    fn bytes_round_trip() {
        let dv = DynamicValue::Bytes(vec![0x00, 0x01, 0xff, 0xfe, 0x80, 0x7f]);
        let sv = dynamic_to_surreal(&dv);
        assert!(matches!(sv, SurrealValue::Bytes(_)));
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, dv);
    }

    #[test]
    fn bytes_round_trip_empty() {
        let dv = DynamicValue::Bytes(Vec::new());
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, dv);
    }

    #[test]
    fn first_oversized_bytes_flags_top_level_violation() {
        use schema_forge_core::types::BytesConstraints;
        let ft = FieldType::Bytes(BytesConstraints::with_max_size(2));
        let val = DynamicValue::Bytes(vec![1, 2, 3]);
        assert_eq!(first_oversized_bytes(&ft, &val), Some((3, 2)));
    }

    #[test]
    fn first_oversized_bytes_ignores_within_limit_and_unconstrained() {
        use schema_forge_core::types::BytesConstraints;
        let capped = FieldType::Bytes(BytesConstraints::with_max_size(8));
        assert_eq!(
            first_oversized_bytes(&capped, &DynamicValue::Bytes(vec![1, 2, 3])),
            None
        );
        let uncapped = FieldType::Bytes(BytesConstraints::unconstrained());
        assert_eq!(
            first_oversized_bytes(&uncapped, &DynamicValue::Bytes(vec![1; 1024])),
            None
        );
    }

    #[test]
    fn first_oversized_bytes_recurses_into_array() {
        use schema_forge_core::types::BytesConstraints;
        let ft = FieldType::Array(Box::new(FieldType::Bytes(BytesConstraints::with_max_size(
            2,
        ))));
        let val = DynamicValue::Array(vec![
            DynamicValue::Bytes(vec![1, 2]),
            DynamicValue::Bytes(vec![1, 2, 3, 4]),
        ]);
        assert_eq!(first_oversized_bytes(&ft, &val), Some((4, 2)));
    }

    #[test]
    fn duration_round_trip_subsecond() {
        let dv = DynamicValue::Duration(
            chrono::TimeDelta::seconds(5) + chrono::TimeDelta::nanoseconds(123_456),
        );
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(back, dv);
    }

    #[test]
    fn first_negative_duration_detects_top_level() {
        let dv = DynamicValue::Duration(chrono::TimeDelta::seconds(-5));
        assert_eq!(
            first_negative_duration(&dv),
            Some(chrono::TimeDelta::seconds(-5))
        );
    }

    #[test]
    fn first_negative_duration_ignores_non_negative() {
        assert_eq!(
            first_negative_duration(&DynamicValue::Duration(chrono::TimeDelta::seconds(5))),
            None
        );
        assert_eq!(
            first_negative_duration(&DynamicValue::Duration(chrono::TimeDelta::zero())),
            None
        );
        assert_eq!(
            first_negative_duration(&DynamicValue::Integer(-9)),
            None,
            "a negative integer is not a duration"
        );
    }

    #[test]
    fn first_negative_duration_recurses_into_array_and_composite() {
        let arr = DynamicValue::Array(vec![
            DynamicValue::Duration(chrono::TimeDelta::seconds(5)),
            DynamicValue::Duration(chrono::TimeDelta::seconds(-3)),
        ]);
        assert_eq!(
            first_negative_duration(&arr),
            Some(chrono::TimeDelta::seconds(-3))
        );

        let mut map = BTreeMap::new();
        map.insert(
            "ttl".to_string(),
            DynamicValue::Duration(chrono::TimeDelta::seconds(-1)),
        );
        let comp = DynamicValue::Composite(map);
        assert_eq!(
            first_negative_duration(&comp),
            Some(chrono::TimeDelta::seconds(-1))
        );
    }

    #[test]
    fn array_round_trip() {
        let dv = DynamicValue::Array(vec![DynamicValue::Integer(1), DynamicValue::Integer(2)]);
        let sv = dynamic_to_surreal(&dv);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(
            back,
            DynamicValue::Array(vec![DynamicValue::Integer(1), DynamicValue::Integer(2),])
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
    fn thing_converts_to_ref() {
        use surrealdb::sql::{Id, Thing};
        let entity_id = EntityId::new("project");
        let thing = Thing::from(("Project", Id::String(entity_id.as_str().to_string())));
        let sv = SurrealValue::Thing(thing);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert!(matches!(back, DynamicValue::Ref(ref id) if id.as_str() == entity_id.as_str()));
    }

    #[test]
    fn thing_non_entity_id_converts_to_text() {
        use surrealdb::sql::{Id, Thing};
        let thing = Thing::from(("SomeTable", Id::String("not_an_entity_id".to_string())));
        let sv = SurrealValue::Thing(thing);
        let back = surreal_to_dynamic(&sv).unwrap();
        assert_eq!(
            back,
            DynamicValue::Text("SomeTable:not_an_entity_id".into())
        );
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
