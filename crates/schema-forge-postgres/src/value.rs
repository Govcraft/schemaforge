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
///
/// When binding a `DynamicValue::Array`, `field_type` should be the column's
/// schema `FieldType::Array(inner)` so the correct native Postgres array type
/// (`text[]`, `bigint[]`, etc.) can be bound. If `field_type` is `None` or the
/// inner element type is not a sqlx-supported primitive, the array is bound
/// as JSONB as a fallback.
pub fn bind_dynamic_value(
    args: &mut PgArguments,
    value: &DynamicValue,
    field_type: Option<&FieldType>,
) -> Result<(), BackendError> {
    match value {
        DynamicValue::Null => {
            bind_null(args, field_type)?;
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
            args.add(id.as_str()).map_err(|e| BackendError::Internal {
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
            bind_array(args, arr, field_type)?;
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

/// Bind a SQL `NULL` for the given column, using the schema's `FieldType`
/// to choose a typed `Option::<T>::None` so PostgreSQL can resolve the
/// parameter type.
///
/// Without this, sqlx infers a `text` parameter from `None::<String>`,
/// which fails with "column X is of type timestamp with time zone but
/// expression is of type text" when the underlying column is anything
/// other than text. This commonly bites the `GET → modify → PUT`
/// round-trip path, where a client sends back a JSON `null` for a typed
/// optional column.
///
/// When `field_type` is `None` (e.g. positional query parameters with no
/// schema context), the function falls back to a `text`-typed null,
/// matching the previous behavior.
fn bind_null(args: &mut PgArguments, field_type: Option<&FieldType>) -> Result<(), BackendError> {
    let result = match field_type {
        Some(FieldType::Integer(_)) => args.add(None::<i64>),
        Some(FieldType::Float(_)) => args.add(None::<f64>),
        Some(FieldType::Boolean) => args.add(None::<bool>),
        Some(FieldType::DateTime) => args.add(None::<chrono::DateTime<chrono::Utc>>),
        Some(FieldType::Json | FieldType::Composite(_)) => {
            args.add(None::<sqlx::types::Json<serde_json::Value>>)
        }
        Some(FieldType::Array(inner)) => return bind_null_array(args, inner),
        // Text, RichText, Enum, Relation (single id stored as text), or
        // unknown column types all serialize as nullable text.
        _ => args.add(None::<String>),
    };
    result.map_err(|e| BackendError::Internal {
        message: format!("failed to bind NULL: {e}"),
    })
}

/// Bind a typed NULL for an array column. Mirrors the per-element matching
/// used by [`bind_array`] so the parameter type matches the column's native
/// Postgres array type (`bigint[]`, `timestamptz[]`, etc.).
fn bind_null_array(args: &mut PgArguments, inner: &FieldType) -> Result<(), BackendError> {
    let result = match inner {
        FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_) => {
            args.add(None::<Vec<String>>)
        }
        FieldType::Integer(_) => args.add(None::<Vec<i64>>),
        FieldType::Float(_) => args.add(None::<Vec<f64>>),
        FieldType::Boolean => args.add(None::<Vec<bool>>),
        FieldType::DateTime => args.add(None::<Vec<chrono::DateTime<chrono::Utc>>>),
        // Nested arrays, composites, relations, etc. are stored as JSONB.
        _ => args.add(None::<sqlx::types::Json<serde_json::Value>>),
    };
    result.map_err(|e| BackendError::Internal {
        message: format!("failed to bind NULL array: {e}"),
    })
}

/// Bind a `DynamicValue::Array` into a `PgArguments` buffer, preferring
/// a native Postgres array when the schema's inner `FieldType` matches a
/// primitive sqlx type. Falls back to JSONB when the schema is unavailable
/// or the inner type is non-primitive (nested arrays, composites, etc.).
fn bind_array(
    args: &mut PgArguments,
    arr: &[DynamicValue],
    field_type: Option<&FieldType>,
) -> Result<(), BackendError> {
    if let Some(FieldType::Array(inner)) = field_type {
        match inner.as_ref() {
            FieldType::Text(_) | FieldType::Enum(_) | FieldType::RichText => {
                let items = array_items_as_strings(arr, inner)?;
                args.add(items).map_err(|e| BackendError::Internal {
                    message: format!("failed to bind text array: {e}"),
                })?;
                return Ok(());
            }
            FieldType::Integer(_) => {
                let items = array_items_as_integers(arr)?;
                args.add(items).map_err(|e| BackendError::Internal {
                    message: format!("failed to bind integer array: {e}"),
                })?;
                return Ok(());
            }
            FieldType::Float(_) => {
                let items = array_items_as_floats(arr)?;
                args.add(items).map_err(|e| BackendError::Internal {
                    message: format!("failed to bind float array: {e}"),
                })?;
                return Ok(());
            }
            FieldType::Boolean => {
                let items = array_items_as_bools(arr)?;
                args.add(items).map_err(|e| BackendError::Internal {
                    message: format!("failed to bind boolean array: {e}"),
                })?;
                return Ok(());
            }
            FieldType::DateTime => {
                let items = array_items_as_datetimes(arr)?;
                args.add(items).map_err(|e| BackendError::Internal {
                    message: format!("failed to bind datetime array: {e}"),
                })?;
                return Ok(());
            }
            // Nested arrays, composites, relations, json, etc. -- fall through to JSONB.
            _ => {}
        }
    }

    // Fallback: bind as JSONB. Used when no schema context is available
    // (e.g. compiled query params) or the inner type is non-primitive.
    let json_val: serde_json::Value = arr
        .iter()
        .map(dynamic_to_json)
        .collect::<Vec<serde_json::Value>>()
        .into();
    args.add(sqlx::types::Json(&json_val))
        .map_err(|e| BackendError::Internal {
            message: format!("failed to bind array: {e}"),
        })?;
    Ok(())
}

fn array_bind_mismatch(expected: &FieldType, got: &DynamicValue) -> BackendError {
    BackendError::Internal {
        message: format!(
            "expected {expected}[] for column array, got {}",
            dynamic_variant_name(got)
        ),
    }
}

fn dynamic_variant_name(value: &DynamicValue) -> &'static str {
    match value {
        DynamicValue::Null => "Null",
        DynamicValue::Text(_) => "Text",
        DynamicValue::Integer(_) => "Integer",
        DynamicValue::Float(_) => "Float",
        DynamicValue::Boolean(_) => "Boolean",
        DynamicValue::DateTime(_) => "DateTime",
        DynamicValue::Enum(_) => "Enum",
        DynamicValue::Json(_) => "Json",
        DynamicValue::Array(_) => "Array",
        DynamicValue::Composite(_) => "Composite",
        DynamicValue::Ref(_) => "Ref",
        DynamicValue::RefArray(_) => "RefArray",
        _ => "Unknown",
    }
}

fn array_items_as_strings(
    arr: &[DynamicValue],
    inner: &FieldType,
) -> Result<Vec<String>, BackendError> {
    arr.iter()
        .map(|item| match item {
            DynamicValue::Text(s) | DynamicValue::Enum(s) => Ok(s.clone()),
            other => Err(array_bind_mismatch(inner, other)),
        })
        .collect()
}

fn array_items_as_integers(arr: &[DynamicValue]) -> Result<Vec<i64>, BackendError> {
    arr.iter()
        .map(|item| match item {
            DynamicValue::Integer(i) => Ok(*i),
            other => Err(array_bind_mismatch(
                &FieldType::Integer(Default::default()),
                other,
            )),
        })
        .collect()
}

fn array_items_as_floats(arr: &[DynamicValue]) -> Result<Vec<f64>, BackendError> {
    arr.iter()
        .map(|item| match item {
            DynamicValue::Float(f) => Ok(*f),
            other => Err(array_bind_mismatch(
                &FieldType::Float(Default::default()),
                other,
            )),
        })
        .collect()
}

fn array_items_as_bools(arr: &[DynamicValue]) -> Result<Vec<bool>, BackendError> {
    arr.iter()
        .map(|item| match item {
            DynamicValue::Boolean(b) => Ok(*b),
            other => Err(array_bind_mismatch(&FieldType::Boolean, other)),
        })
        .collect()
}

fn array_items_as_datetimes(
    arr: &[DynamicValue],
) -> Result<Vec<chrono::DateTime<chrono::Utc>>, BackendError> {
    arr.iter()
        .map(|item| match item {
            DynamicValue::DateTime(dt) => Ok(*dt),
            other => Err(array_bind_mismatch(&FieldType::DateTime, other)),
        })
        .collect()
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

        let field_type = schema_def
            .and_then(|sd| sd.field(col_name))
            .map(|fd| &fd.field_type);
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
            let v: Vec<String> = row.try_get(col_name).map_err(|e| BackendError::Internal {
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
        Some(FieldType::Array(inner)) => read_array_column(row, col_name, inner),
        // Text, RichText, or unknown -- read as string
        _ => {
            let v: String = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read text column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Text(v))
        }
    }
}

/// Read an array column from a PostgreSQL row, using the schema's inner field
/// type to select the correct native Postgres array decoding. Falls back to
/// JSONB for non-primitive inner types (nested arrays, composites, etc.).
fn read_array_column(
    row: &PgRow,
    col_name: &str,
    inner: &FieldType,
) -> Result<DynamicValue, BackendError> {
    match inner {
        FieldType::Text(_) | FieldType::Enum(_) | FieldType::RichText => {
            let v: Vec<String> = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read text array column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Array(
                v.into_iter().map(DynamicValue::Text).collect(),
            ))
        }
        FieldType::Integer(_) => {
            let v: Vec<i64> = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read integer array column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Array(
                v.into_iter().map(DynamicValue::Integer).collect(),
            ))
        }
        FieldType::Float(_) => {
            let v: Vec<f64> = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read float array column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Array(
                v.into_iter().map(DynamicValue::Float).collect(),
            ))
        }
        FieldType::Boolean => {
            let v: Vec<bool> = row.try_get(col_name).map_err(|e| BackendError::Internal {
                message: format!("failed to read boolean array column '{col_name}': {e}"),
            })?;
            Ok(DynamicValue::Array(
                v.into_iter().map(DynamicValue::Boolean).collect(),
            ))
        }
        FieldType::DateTime => {
            let v: Vec<chrono::DateTime<chrono::Utc>> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read datetime array column '{col_name}': {e}"),
                })?;
            Ok(DynamicValue::Array(
                v.into_iter().map(DynamicValue::DateTime).collect(),
            ))
        }
        // Nested arrays, composites, relations, json, etc. -- fall back to JSONB.
        _ => {
            let v: sqlx::types::Json<serde_json::Value> =
                row.try_get(col_name).map_err(|e| BackendError::Internal {
                    message: format!("failed to read array column '{col_name}': {e}"),
                })?;
            Ok(json_to_dynamic_array(&v.0))
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
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, None).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Text("hello".into()), None).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Integer(42), None).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Float(2.5), None).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Boolean(true), None).is_ok());

        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Enum("Active".into()), None).is_ok());
    }

    #[test]
    fn bind_array_with_text_field_type_binds_native_text_array() {
        let mut args = PgArguments::default();
        let arr = DynamicValue::Array(vec![
            DynamicValue::Text("rust".into()),
            DynamicValue::Text("aws".into()),
        ]);
        let ft = FieldType::Array(Box::new(FieldType::Text(
            schema_forge_core::types::TextConstraints::default(),
        )));
        assert!(bind_dynamic_value(&mut args, &arr, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_array_with_integer_field_type_binds_native_integer_array() {
        let mut args = PgArguments::default();
        let arr = DynamicValue::Array(vec![DynamicValue::Integer(1), DynamicValue::Integer(2)]);
        let ft = FieldType::Array(Box::new(FieldType::Integer(
            schema_forge_core::types::IntegerConstraints::default(),
        )));
        assert!(bind_dynamic_value(&mut args, &arr, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_array_with_boolean_field_type_binds_native_boolean_array() {
        let mut args = PgArguments::default();
        let arr = DynamicValue::Array(vec![
            DynamicValue::Boolean(true),
            DynamicValue::Boolean(false),
        ]);
        let ft = FieldType::Array(Box::new(FieldType::Boolean));
        assert!(bind_dynamic_value(&mut args, &arr, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_array_without_field_type_falls_back_to_jsonb() {
        let mut args = PgArguments::default();
        let arr = DynamicValue::Array(vec![DynamicValue::Text("a".into())]);
        assert!(bind_dynamic_value(&mut args, &arr, None).is_ok());
    }

    #[test]
    fn bind_null_with_datetime_field_type_uses_typed_none() {
        // Regression test for issue #10: GET → modify → PUT round-trips
        // failed because Null was bound as text, producing
        // "column X is of type timestamp with time zone but expression is
        // of type text". With the fix, the parameter is encoded as
        // Option::<DateTime<Utc>>::None instead.
        let mut args = PgArguments::default();
        assert!(
            bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&FieldType::DateTime)).is_ok()
        );
    }

    #[test]
    fn bind_null_with_integer_field_type_uses_typed_none() {
        let mut args = PgArguments::default();
        let ft = FieldType::Integer(schema_forge_core::types::IntegerConstraints::default());
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_null_with_float_field_type_uses_typed_none() {
        let mut args = PgArguments::default();
        let ft = FieldType::Float(schema_forge_core::types::FloatConstraints::default());
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_null_with_boolean_field_type_uses_typed_none() {
        let mut args = PgArguments::default();
        assert!(
            bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&FieldType::Boolean)).is_ok()
        );
    }

    #[test]
    fn bind_null_with_text_field_type_uses_typed_none() {
        let mut args = PgArguments::default();
        let ft = FieldType::Text(schema_forge_core::types::TextConstraints::default());
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_null_with_json_field_type_uses_typed_none() {
        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&FieldType::Json)).is_ok());
    }

    #[test]
    fn bind_null_with_array_field_type_uses_typed_none() {
        let mut args = PgArguments::default();
        let ft = FieldType::Array(Box::new(FieldType::Integer(
            schema_forge_core::types::IntegerConstraints::default(),
        )));
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, Some(&ft)).is_ok());
    }

    #[test]
    fn bind_null_without_field_type_uses_text_fallback() {
        // Backwards-compatible behavior: positional query parameters
        // without schema context still bind as nullable text.
        let mut args = PgArguments::default();
        assert!(bind_dynamic_value(&mut args, &DynamicValue::Null, None).is_ok());
    }

    #[test]
    fn bind_array_type_mismatch_returns_internal_error() {
        let mut args = PgArguments::default();
        let arr = DynamicValue::Array(vec![DynamicValue::Integer(1)]);
        let ft = FieldType::Array(Box::new(FieldType::Text(
            schema_forge_core::types::TextConstraints::default(),
        )));
        let err = bind_dynamic_value(&mut args, &arr, Some(&ft))
            .expect_err("expected type mismatch error");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("expected"),
            "error should describe type mismatch, got: {msg}"
        );
    }
}
