use std::collections::BTreeMap;

use async_graphql::dynamic::TypeRef;
use async_graphql::indexmap;
use async_graphql::Value as GqlValue;
use async_graphql::{Name, Number};
use schema_forge_core::types::{Cardinality, DynamicValue, FieldType, IntegerConstraints};

/// Custom scalar names used in the GraphQL schema.
pub const DATETIME_SCALAR: &str = "DateTime";
pub const JSON_SCALAR: &str = "JSON";
pub const INT64_SCALAR: &str = "Int64";

/// Check whether integer constraints fit within i32 range.
fn fits_i32(constraints: &IntegerConstraints) -> bool {
    let min_ok = constraints.min.is_some_and(|m| m >= i64::from(i32::MIN));
    let max_ok = constraints.max.is_some_and(|m| m <= i64::from(i32::MAX));
    min_ok && max_ok
}

/// Map a `FieldType` to an async-graphql `TypeRef`.
///
/// `schema_name` and `field_name` are used to generate unique type names for
/// enums and composites (e.g. `Contact_status` for an enum on Contact.status).
pub fn field_type_to_type_ref(
    schema_name: &str,
    field_name: &str,
    ft: &FieldType,
    required: bool,
) -> TypeRef {
    let base = match ft {
        FieldType::Text(_) | FieldType::RichText => TypeRef::named(TypeRef::STRING),
        FieldType::Integer(constraints) => {
            if fits_i32(constraints) {
                TypeRef::named(TypeRef::INT)
            } else {
                TypeRef::named(INT64_SCALAR)
            }
        }
        FieldType::Float(_) => TypeRef::named(TypeRef::FLOAT),
        FieldType::Boolean => TypeRef::named(TypeRef::BOOLEAN),
        FieldType::DateTime => TypeRef::named(DATETIME_SCALAR),
        FieldType::Enum(_) => {
            let type_name = format!("{schema_name}_{field_name}");
            TypeRef::named(type_name)
        }
        FieldType::Json => TypeRef::named(JSON_SCALAR),
        FieldType::Relation {
            target,
            cardinality,
        } => {
            let target_name = target.as_str().to_string();
            return match cardinality {
                Cardinality::One => TypeRef::named(target_name),
                Cardinality::Many => TypeRef::named_nn_list(target_name),
                _ => TypeRef::named(target_name),
            };
        }
        FieldType::Array(inner) => {
            let inner_ref = field_type_to_type_ref(schema_name, field_name, inner, false);
            return if required {
                TypeRef::named_nn_list(inner_ref_to_name(&inner_ref))
            } else {
                TypeRef::named_list(inner_ref_to_name(&inner_ref))
            };
        }
        FieldType::Composite(_) => {
            let type_name = format!("{schema_name}_{field_name}");
            TypeRef::named(type_name)
        }
        _ => TypeRef::named(TypeRef::STRING),
    };

    if required {
        make_non_null(&base)
    } else {
        base
    }
}

/// Extract the type name from a TypeRef (stripping list/non-null wrappers).
fn inner_ref_to_name(type_ref: &TypeRef) -> &str {
    // TypeRef is opaque, so we rely on its Display. For named types, the name is the string.
    // Since we only use TypeRef::named(...) for inner types, we can match on common scalars.
    // async-graphql's TypeRef::named returns the name directly.
    // We need the raw name for wrapping in a list. Use the type_name() method.
    type_ref_name(type_ref)
}

fn type_ref_name(tr: &TypeRef) -> &str {
    // TypeRef stores the name — we need to extract it.
    // The TypeRef debug/display will give us something like "String" or "String!".
    // Since async-graphql 7.x TypeRef is just a string wrapper, we can convert.
    // Actually, TypeRef in dynamic mode is just a newtype around String.
    // Let's use the fact that TypeRef implements AsRef<str> or Display.
    // Fallback: just use TypeRef::STRING etc.
    // Since we can't easily extract the inner name, we'll restructure to pass names directly.
    // For now, this function is only called for Array inner types.
    let s = format!("{tr}");
    // Strip trailing ! for non-null
    let s = s.trim_end_matches('!');
    // This is a leak but only happens at schema build time (finite schema definitions)
    // and avoids complex lifetime gymnastics with TypeRef.
    Box::leak(s.to_string().into_boxed_str())
}

fn make_non_null(tr: &TypeRef) -> TypeRef {
    TypeRef::named_nn(type_ref_name(tr))
}

/// Convert a `DynamicValue` to an `async_graphql::Value`, using the field type
/// to determine how to serialize integers (Int vs Int64).
pub fn dynamic_value_to_gql_value(dv: &DynamicValue, field_type: Option<&FieldType>) -> GqlValue {
    match dv {
        DynamicValue::Null => GqlValue::Null,
        DynamicValue::Text(s) => GqlValue::String(s.clone()),
        DynamicValue::Enum(s) => GqlValue::Enum(Name::new(s)),
        DynamicValue::Integer(i) => {
            let use_int64 = match field_type {
                Some(FieldType::Integer(c)) => !fits_i32(c),
                _ => false,
            };
            if use_int64 {
                GqlValue::String(i.to_string())
            } else {
                GqlValue::Number(Number::from(*i))
            }
        }
        DynamicValue::Float(f) => Number::from_f64(*f)
            .map(GqlValue::Number)
            .unwrap_or(GqlValue::Null),
        DynamicValue::Boolean(b) => GqlValue::Boolean(*b),
        DynamicValue::DateTime(dt) => GqlValue::String(dt.to_rfc3339()),
        DynamicValue::Json(v) => json_to_gql_value(v),
        DynamicValue::Array(arr) => {
            let inner_type = match field_type {
                Some(FieldType::Array(inner)) => Some(inner.as_ref()),
                _ => None,
            };
            GqlValue::List(
                arr.iter()
                    .map(|item| dynamic_value_to_gql_value(item, inner_type))
                    .collect(),
            )
        }
        DynamicValue::Composite(map) => {
            let obj = map
                .iter()
                .map(|(k, v)| (Name::new(k), dynamic_value_to_gql_value(v, None)))
                .collect();
            GqlValue::Object(obj)
        }
        DynamicValue::Ref(id) => GqlValue::String(id.as_str().to_string()),
        DynamicValue::RefArray(ids) => GqlValue::List(
            ids.iter()
                .map(|id| GqlValue::String(id.as_str().to_string()))
                .collect(),
        ),
        _ => GqlValue::Null,
    }
}

/// Convert a serde_json::Value to an async_graphql::Value.
pub fn json_to_gql_value(v: &serde_json::Value) -> GqlValue {
    match v {
        serde_json::Value::Null => GqlValue::Null,
        serde_json::Value::Bool(b) => GqlValue::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                GqlValue::Number(Number::from(i))
            } else if let Some(f) = n.as_f64() {
                Number::from_f64(f)
                    .map(GqlValue::Number)
                    .unwrap_or(GqlValue::Null)
            } else {
                GqlValue::Null
            }
        }
        serde_json::Value::String(s) => GqlValue::String(s.clone()),
        serde_json::Value::Array(arr) => {
            GqlValue::List(arr.iter().map(json_to_gql_value).collect())
        }
        serde_json::Value::Object(map) => {
            let obj = map
                .iter()
                .map(|(k, v)| (Name::new(k), json_to_gql_value(v)))
                .collect();
            GqlValue::Object(obj)
        }
    }
}

/// Convert a GraphQL input value back to a serde_json::Value for reuse with
/// `json_to_entity_fields`.
pub fn gql_value_to_json(v: &GqlValue) -> serde_json::Value {
    match v {
        GqlValue::Null => serde_json::Value::Null,
        GqlValue::Boolean(b) => serde_json::Value::Bool(*b),
        GqlValue::Number(n) => {
            // async_graphql::Number wraps serde_json::Number internally
            serde_json::Value::Number(n.clone())
        }
        GqlValue::String(s) => serde_json::Value::String(s.clone()),
        GqlValue::Enum(name) => serde_json::Value::String(name.to_string()),
        GqlValue::List(arr) => {
            serde_json::Value::Array(arr.iter().map(gql_value_to_json).collect())
        }
        GqlValue::Object(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.to_string(), gql_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

/// Convert a `BTreeMap<String, DynamicValue>` to a GraphQL object value,
/// looking up field types from the provided field map.
pub fn entity_fields_to_gql_object(
    fields: &BTreeMap<String, DynamicValue>,
    schema_fields: &[schema_forge_core::types::FieldDefinition],
) -> indexmap::IndexMap<Name, GqlValue> {
    let mut obj = indexmap::IndexMap::new();
    for (key, value) in fields {
        let field_type = schema_fields
            .iter()
            .find(|fd| fd.name.as_str() == key)
            .map(|fd| &fd.field_type);
        obj.insert(
            Name::new(key),
            dynamic_value_to_gql_value(value, field_type),
        );
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::FloatConstraints;

    #[test]
    fn fits_i32_constrained() {
        let c = IntegerConstraints::with_range(0, 100).unwrap();
        assert!(fits_i32(&c));
    }

    #[test]
    fn fits_i32_unconstrained() {
        let c = IntegerConstraints::unconstrained();
        assert!(!fits_i32(&c));
    }

    #[test]
    fn fits_i32_exceeds_max() {
        let c = IntegerConstraints::with_range(0, i64::from(i32::MAX) + 1).unwrap();
        assert!(!fits_i32(&c));
    }

    #[test]
    fn fits_i32_exceeds_min() {
        let c = IntegerConstraints::with_range(i64::from(i32::MIN) - 1, 0).unwrap();
        assert!(!fits_i32(&c));
    }

    #[test]
    fn text_maps_to_string() {
        use schema_forge_core::types::TextConstraints;
        let tr = field_type_to_type_ref(
            "Test",
            "name",
            &FieldType::Text(TextConstraints::unconstrained()),
            false,
        );
        assert_eq!(format!("{tr}"), "String");
    }

    #[test]
    fn integer_constrained_maps_to_int() {
        let c = IntegerConstraints::with_range(0, 100).unwrap();
        let tr = field_type_to_type_ref("Test", "count", &FieldType::Integer(c), false);
        assert_eq!(format!("{tr}"), "Int");
    }

    #[test]
    fn integer_unconstrained_maps_to_int64() {
        let c = IntegerConstraints::unconstrained();
        let tr = field_type_to_type_ref("Test", "big_id", &FieldType::Integer(c), false);
        assert_eq!(format!("{tr}"), INT64_SCALAR);
    }

    #[test]
    fn float_maps_to_float() {
        let tr = field_type_to_type_ref(
            "Test",
            "score",
            &FieldType::Float(FloatConstraints::unconstrained()),
            false,
        );
        assert_eq!(format!("{tr}"), "Float");
    }

    #[test]
    fn boolean_maps_to_boolean() {
        let tr = field_type_to_type_ref("Test", "active", &FieldType::Boolean, false);
        assert_eq!(format!("{tr}"), "Boolean");
    }

    #[test]
    fn datetime_maps_to_scalar() {
        let tr = field_type_to_type_ref("Test", "created", &FieldType::DateTime, false);
        assert_eq!(format!("{tr}"), DATETIME_SCALAR);
    }

    #[test]
    fn json_maps_to_scalar() {
        let tr = field_type_to_type_ref("Test", "meta", &FieldType::Json, false);
        assert_eq!(format!("{tr}"), JSON_SCALAR);
    }

    #[test]
    fn required_produces_non_null() {
        use schema_forge_core::types::TextConstraints;
        let tr = field_type_to_type_ref(
            "Test",
            "name",
            &FieldType::Text(TextConstraints::unconstrained()),
            true,
        );
        let s = format!("{tr}");
        assert!(s.ends_with('!'));
    }

    #[test]
    fn relation_one_always_nullable() {
        use schema_forge_core::types::SchemaName;
        let tr = field_type_to_type_ref(
            "Contact",
            "company",
            &FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::One,
            },
            false, // Relations are always passed as non-required
        );
        let s = format!("{tr}");
        assert!(!s.ends_with('!'));
    }

    #[test]
    fn dv_null_to_gql() {
        assert_eq!(
            dynamic_value_to_gql_value(&DynamicValue::Null, None),
            GqlValue::Null
        );
    }

    #[test]
    fn dv_text_to_gql() {
        let v = dynamic_value_to_gql_value(&DynamicValue::Text("hello".into()), None);
        assert_eq!(v, GqlValue::String("hello".into()));
    }

    #[test]
    fn dv_integer_to_gql_int() {
        let c = IntegerConstraints::with_range(0, 100).unwrap();
        let v =
            dynamic_value_to_gql_value(&DynamicValue::Integer(42), Some(&FieldType::Integer(c)));
        assert_eq!(v, GqlValue::Number(Number::from(42)));
    }

    #[test]
    fn dv_integer_to_gql_int64() {
        let c = IntegerConstraints::unconstrained();
        let v = dynamic_value_to_gql_value(
            &DynamicValue::Integer(i64::MAX),
            Some(&FieldType::Integer(c)),
        );
        assert_eq!(v, GqlValue::String(i64::MAX.to_string()));
    }

    #[test]
    fn dv_boolean_to_gql() {
        let v = dynamic_value_to_gql_value(&DynamicValue::Boolean(true), None);
        assert_eq!(v, GqlValue::Boolean(true));
    }

    #[test]
    fn gql_value_roundtrip_json() {
        let json = serde_json::json!({"a": 1, "b": "two", "c": [true, null]});
        let gql = json_to_gql_value(&json);
        let back = gql_value_to_json(&gql);
        assert_eq!(json, back);
    }
}
