//! The CEL ↔ [`DynamicValue`] bridge.
//!
//! Two pure, total-where-possible conversions move values across the boundary
//! between SchemaForge's storage domain ([`DynamicValue`]) and the CEL engine's
//! own value model ([`CelValue`]):
//!
//! - [`dynamic_to_cel`] surfaces a stored field to a predicate. It never fails
//!   for any variant defined today; the `Result` exists only so a future
//!   `DynamicValue` variant (the enum is `#[non_exhaustive]`) can be reported
//!   via [`ConversionError::Unsupported`] instead of silently mishandled.
//! - [`cel_to_dynamic`] writes a CEL result back into storage. It is the
//!   natural inverse and is used by `@compute`/`@default` (#93/#94). A handful
//!   of CEL-internal types (`bytes`, `duration`, `type`) have no `DynamicValue`
//!   representation yet (tracked by #96/#97), so they convert to
//!   [`ConversionError::Unsupported`].
//!
//! Refs are surfaced to predicates as their id strings: a `Ref` becomes the id
//! string, a `RefArray` becomes a list of id strings. This lets predicates
//! compare and inspect references without the engine needing a dedicated
//! ref type.

use std::collections::BTreeMap;

use schema_forge_core::types::DynamicValue;

use crate::error::ConversionError;
use crate::value::{CelKey, CelValue};

/// Convert a stored [`DynamicValue`] into a [`CelValue`] for predicate
/// evaluation.
///
/// This direction is total for every variant defined today and therefore never
/// returns `Err` in practice; the `Result` is retained because `DynamicValue`
/// is `#[non_exhaustive]` and a future variant must fail closed rather than be
/// silently dropped.
pub fn dynamic_to_cel(v: &DynamicValue) -> Result<CelValue, ConversionError> {
    match v {
        DynamicValue::Null => Ok(CelValue::Null),
        DynamicValue::Text(s) => Ok(CelValue::String(s.clone())),
        DynamicValue::Integer(i) => Ok(CelValue::Int(*i)),
        DynamicValue::Float(f) => Ok(CelValue::Double(*f)),
        DynamicValue::Boolean(b) => Ok(CelValue::Bool(*b)),
        DynamicValue::DateTime(dt) => Ok(CelValue::Timestamp(*dt)),
        DynamicValue::Duration(d) => Ok(CelValue::Duration(*d)),
        DynamicValue::Bytes(b) => Ok(CelValue::Bytes(b.clone())),
        DynamicValue::Enum(s) => Ok(CelValue::String(s.clone())),
        DynamicValue::Json(j) => json_to_cel(j),
        DynamicValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(dynamic_to_cel(item)?);
            }
            Ok(CelValue::List(out))
        }
        DynamicValue::Composite(map) => {
            let mut out = BTreeMap::new();
            for (k, value) in map {
                out.insert(CelKey::String(k.clone()), dynamic_to_cel(value)?);
            }
            Ok(CelValue::Map(out))
        }
        DynamicValue::Ref(id) => Ok(CelValue::String(id.as_str().to_string())),
        DynamicValue::RefArray(ids) => {
            let out = ids
                .iter()
                .map(|id| CelValue::String(id.as_str().to_string()))
                .collect();
            Ok(CelValue::List(out))
        }
        // `DynamicValue` is `#[non_exhaustive]`: a variant added in the future
        // has no defined CEL mapping yet, so fail closed.
        other => Err(ConversionError::Unsupported(format!(
            "DynamicValue variant has no CEL mapping: {other:?}"
        ))),
    }
}

/// Convert a `serde_json::Value` (carried inside [`DynamicValue::Json`]) into a
/// [`CelValue`].
///
/// JSON numbers map to `Int` when they are an exact `i64`, otherwise to
/// `Double`. Objects become maps with `string` keys.
fn json_to_cel(j: &serde_json::Value) -> Result<CelValue, ConversionError> {
    match j {
        serde_json::Value::Null => Ok(CelValue::Null),
        serde_json::Value::Bool(b) => Ok(CelValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(CelValue::Int(i))
            } else {
                // Covers u64 > i64::MAX and all non-integral numbers.
                let f = n.as_f64().ok_or_else(|| {
                    ConversionError::Unsupported(format!("JSON number is not representable: {n}"))
                })?;
                Ok(CelValue::Double(f))
            }
        }
        serde_json::Value::String(s) => Ok(CelValue::String(s.clone())),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_cel(item)?);
            }
            Ok(CelValue::List(out))
        }
        serde_json::Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (k, value) in map {
                out.insert(CelKey::String(k.clone()), json_to_cel(value)?);
            }
            Ok(CelValue::Map(out))
        }
    }
}

/// Convert a [`CelValue`] (e.g. the result of a `@compute`/`@default`
/// expression) back into a [`DynamicValue`] for storage.
///
/// CEL-internal types without a storage representation (`bytes`, `duration`,
/// `type`) and unsigned integers that overflow `i64` produce a
/// [`ConversionError`].
pub fn cel_to_dynamic(v: &CelValue) -> Result<DynamicValue, ConversionError> {
    match v {
        CelValue::Null => Ok(DynamicValue::Null),
        CelValue::Bool(b) => Ok(DynamicValue::Boolean(*b)),
        CelValue::Int(i) => Ok(DynamicValue::Integer(*i)),
        CelValue::Uint(u) => i64::try_from(*u)
            .map(DynamicValue::Integer)
            .map_err(|_| ConversionError::Overflow(format!("uint {u} exceeds i64::MAX"))),
        CelValue::Double(f) => Ok(DynamicValue::Float(*f)),
        CelValue::String(s) => Ok(DynamicValue::Text(s.clone())),
        CelValue::Timestamp(dt) => Ok(DynamicValue::DateTime(*dt)),
        CelValue::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(cel_to_dynamic(item)?);
            }
            Ok(DynamicValue::Array(out))
        }
        CelValue::Map(map) => {
            let mut out = BTreeMap::new();
            for (k, value) in map {
                let key = match k {
                    CelKey::String(s) => s.clone(),
                    _ => {
                        return Err(ConversionError::Unsupported(
                            "non-string map key".to_string(),
                        ))
                    }
                };
                out.insert(key, cel_to_dynamic(value)?);
            }
            Ok(DynamicValue::Composite(out))
        }
        CelValue::Bytes(b) => Ok(DynamicValue::Bytes(b.clone())),
        CelValue::Duration(d) => Ok(DynamicValue::Duration(*d)),
        CelValue::Type(_) => Err(ConversionError::Unsupported("type".to_string())),
        // A present optional unwraps to its inner value (recursively); an absent
        // optional has no storage representation and fails closed — we never
        // fabricate a value for `optional.none()`.
        CelValue::Optional(Some(inner)) => cel_to_dynamic(inner),
        CelValue::Optional(None) => Err(ConversionError::Unsupported(
            "optional.none() has no DynamicValue representation".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use schema_forge_core::types::EntityId;

    #[test]
    fn roundtrip_null() {
        let d = DynamicValue::Null;
        assert_eq!(dynamic_to_cel(&d).unwrap(), CelValue::Null);
        assert_eq!(cel_to_dynamic(&CelValue::Null).unwrap(), d);
    }

    #[test]
    fn roundtrip_text() {
        let d = DynamicValue::Text("hello".to_string());
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::String("hello".to_string()));
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn roundtrip_integer() {
        let d = DynamicValue::Integer(42);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::Int(42));
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn roundtrip_float() {
        let d = DynamicValue::Float(2.5);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::Double(2.5));
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn roundtrip_boolean() {
        let d = DynamicValue::Boolean(true);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::Bool(true));
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn roundtrip_datetime() {
        let dt = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
        let d = DynamicValue::DateTime(dt);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::Timestamp(dt));
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn roundtrip_duration() {
        let d = DynamicValue::Duration(chrono::TimeDelta::seconds(220_752_000));
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(
            c,
            CelValue::Duration(chrono::TimeDelta::seconds(220_752_000))
        );
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn roundtrip_bytes() {
        let d = DynamicValue::Bytes(vec![0x00, 0x01, 0xff, 0xfe, 0x80]);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::Bytes(vec![0x00, 0x01, 0xff, 0xfe, 0x80]));
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn enum_maps_to_string() {
        let d = DynamicValue::Enum("Active".to_string());
        assert_eq!(
            dynamic_to_cel(&d).unwrap(),
            CelValue::String("Active".to_string())
        );
    }

    #[test]
    fn nested_array() {
        let d = DynamicValue::Array(vec![
            DynamicValue::Integer(1),
            DynamicValue::Text("two".to_string()),
        ]);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(
            c,
            CelValue::List(vec![
                CelValue::Int(1),
                CelValue::String("two".to_string())
            ])
        );
        // Inverse comes back as an Array of the same values.
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn nested_composite() {
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), DynamicValue::Integer(1));
        map.insert("b".to_string(), DynamicValue::Boolean(false));
        let d = DynamicValue::Composite(map);
        let c = dynamic_to_cel(&d).unwrap();

        let mut expected = BTreeMap::new();
        expected.insert(CelKey::String("a".to_string()), CelValue::Int(1));
        expected.insert(CelKey::String("b".to_string()), CelValue::Bool(false));
        assert_eq!(c, CelValue::Map(expected));

        // Map round-trips back to a Composite.
        assert_eq!(cel_to_dynamic(&c).unwrap(), d);
    }

    #[test]
    fn json_object_to_map() {
        let d = DynamicValue::Json(serde_json::json!({
            "count": 3,
            "ratio": 1.5,
            "label": "x",
            "ok": true,
            "tags": ["a", "b"],
            "nothing": null,
        }));
        let c = dynamic_to_cel(&d).unwrap();
        let CelValue::Map(map) = c else {
            panic!("expected map, got {c:?}");
        };
        assert_eq!(map[&CelKey::String("count".to_string())], CelValue::Int(3));
        assert_eq!(
            map[&CelKey::String("ratio".to_string())],
            CelValue::Double(1.5)
        );
        assert_eq!(
            map[&CelKey::String("label".to_string())],
            CelValue::String("x".to_string())
        );
        assert_eq!(map[&CelKey::String("ok".to_string())], CelValue::Bool(true));
        assert_eq!(
            map[&CelKey::String("tags".to_string())],
            CelValue::List(vec![
                CelValue::String("a".to_string()),
                CelValue::String("b".to_string())
            ])
        );
        assert_eq!(map[&CelKey::String("nothing".to_string())], CelValue::Null);
    }

    #[test]
    fn json_large_number_is_double() {
        // u64 above i64::MAX cannot be an i64; it must surface as a Double.
        let big = serde_json::json!(u64::MAX);
        let d = DynamicValue::Json(big);
        let c = dynamic_to_cel(&d).unwrap();
        assert!(matches!(c, CelValue::Double(_)));
    }

    #[test]
    fn ref_maps_to_id_string() {
        let id = EntityId::new("contact");
        let d = DynamicValue::Ref(id.clone());
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(c, CelValue::String(id.as_str().to_string()));
    }

    #[test]
    fn ref_array_maps_to_string_list() {
        let a = EntityId::new("contact");
        let b = EntityId::new("contact");
        let d = DynamicValue::RefArray(vec![a.clone(), b.clone()]);
        let c = dynamic_to_cel(&d).unwrap();
        assert_eq!(
            c,
            CelValue::List(vec![
                CelValue::String(a.as_str().to_string()),
                CelValue::String(b.as_str().to_string()),
            ])
        );
    }

    #[test]
    fn uint_within_range_converts() {
        let c = CelValue::Uint(7);
        assert_eq!(cel_to_dynamic(&c).unwrap(), DynamicValue::Integer(7));
    }

    #[test]
    fn uint_overflow_errors() {
        let c = CelValue::Uint(u64::MAX);
        let err = cel_to_dynamic(&c).unwrap_err();
        assert!(matches!(err, ConversionError::Overflow(_)));
    }

    #[test]
    fn cel_bytes_maps_to_dynamic_bytes() {
        let got = cel_to_dynamic(&CelValue::Bytes(vec![1, 2, 3])).unwrap();
        assert_eq!(got, DynamicValue::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn cel_duration_maps_to_dynamic_duration() {
        let got = cel_to_dynamic(&CelValue::Duration(chrono::TimeDelta::seconds(1))).unwrap();
        assert_eq!(got, DynamicValue::Duration(chrono::TimeDelta::seconds(1)));
    }

    #[test]
    fn type_unsupported() {
        let err = cel_to_dynamic(&CelValue::Type("int".to_string())).unwrap_err();
        assert_eq!(err, ConversionError::Unsupported("type".to_string()));
    }

    #[test]
    fn non_string_map_key_unsupported() {
        let mut map = BTreeMap::new();
        map.insert(CelKey::Int(1), CelValue::Bool(true));
        let err = cel_to_dynamic(&CelValue::Map(map)).unwrap_err();
        assert_eq!(
            err,
            ConversionError::Unsupported("non-string map key".to_string())
        );
    }
}
