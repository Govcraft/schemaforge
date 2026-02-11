use async_graphql::dynamic::{Enum, InputObject, InputValue, TypeRef};
use async_graphql::indexmap;
use async_graphql::Value as GqlValue;
use schema_forge_core::query::{FieldPath, Filter};
use schema_forge_core::types::{Cardinality, FieldType, SchemaDefinition};

use super::type_mapping::{gql_value_to_json, DATETIME_SCALAR, JSON_SCALAR};
use crate::routes::entities::json_to_entity_fields;

/// Shared SortOrder enum (registered once).
pub const SORT_ORDER_ENUM: &str = "SortOrder";

/// Build the shared `SortOrder` enum.
pub fn build_sort_order_enum() -> Enum {
    Enum::new(SORT_ORDER_ENUM)
        .item(async_graphql::dynamic::EnumItem::new("ASC"))
        .item(async_graphql::dynamic::EnumItem::new("DESC"))
}

/// Build a `Create{Schema}Input` input object.
pub fn build_create_input(schema: &SchemaDefinition) -> InputObject {
    let name = format!("Create{}Input", schema.name.as_str());
    let mut input = InputObject::new(&name);

    for field in &schema.fields {
        let field_name = field.name.as_str();
        let required = field.is_required();
        let type_ref = input_field_type_ref(schema.name.as_str(), field_name, &field.field_type, required);
        input = input.field(InputValue::new(field_name, type_ref));
    }

    input
}

/// Build an `Update{Schema}Input` input object (all fields nullable).
pub fn build_update_input(schema: &SchemaDefinition) -> InputObject {
    let name = format!("Update{}Input", schema.name.as_str());
    let mut input = InputObject::new(&name);

    for field in &schema.fields {
        let field_name = field.name.as_str();
        let type_ref = input_field_type_ref(schema.name.as_str(), field_name, &field.field_type, false);
        input = input.field(InputValue::new(field_name, type_ref));
    }

    input
}

/// Build a `{Schema}Filter` input object.
pub fn build_filter_input(schema: &SchemaDefinition) -> InputObject {
    let schema_name = schema.name.as_str();
    let filter_name = format!("{schema_name}Filter");
    let mut input = InputObject::new(&filter_name);

    for field in &schema.fields {
        let field_name = field.name.as_str();
        let ops = filter_ops_for_type(&field.field_type);
        for op in ops {
            let key = format!("{field_name}_{op}");
            let type_ref = filter_value_type_ref(schema_name, field_name, &field.field_type, op);
            input = input.field(InputValue::new(key, type_ref));
        }
    }

    // Logical operators
    input = input.field(InputValue::new(
        "and",
        TypeRef::named_list(&filter_name),
    ));
    input = input.field(InputValue::new(
        "or",
        TypeRef::named_list(&filter_name),
    ));
    input = input.field(InputValue::new("not", TypeRef::named(&filter_name)));

    input
}

/// Build a `{Schema}SortField` enum.
pub fn build_sort_field_enum(schema: &SchemaDefinition) -> Enum {
    let name = format!("{}SortField", schema.name.as_str());
    let mut e = Enum::new(&name);
    for field in &schema.fields {
        e = e.item(async_graphql::dynamic::EnumItem::new(field.name.as_str()));
    }
    e
}

/// Build a `{Schema}SortInput` input object.
pub fn build_sort_input(schema: &SchemaDefinition) -> InputObject {
    let schema_name = schema.name.as_str();
    let sort_field_enum = format!("{schema_name}SortField");
    let name = format!("{schema_name}SortInput");
    InputObject::new(&name)
        .field(InputValue::new("field", TypeRef::named_nn(&sort_field_enum)))
        .field(InputValue::new("order", TypeRef::named(SORT_ORDER_ENUM)))
}

/// Map a FieldType to an input TypeRef. Similar to output but uses ID for relations.
fn input_field_type_ref(
    schema_name: &str,
    field_name: &str,
    ft: &FieldType,
    required: bool,
) -> TypeRef {
    let base = match ft {
        FieldType::Text(_) | FieldType::RichText => TypeRef::named(TypeRef::STRING),
        FieldType::Integer(_) => TypeRef::named(TypeRef::INT),
        FieldType::Float(_) => TypeRef::named(TypeRef::FLOAT),
        FieldType::Boolean => TypeRef::named(TypeRef::BOOLEAN),
        FieldType::DateTime => TypeRef::named(DATETIME_SCALAR),
        FieldType::Enum(_) => {
            let type_name = format!("{schema_name}_{field_name}");
            TypeRef::named(type_name)
        }
        FieldType::Json => TypeRef::named(JSON_SCALAR),
        FieldType::Relation { cardinality, .. } => {
            return match cardinality {
                Cardinality::One => {
                    if required {
                        TypeRef::named_nn(TypeRef::ID)
                    } else {
                        TypeRef::named(TypeRef::ID)
                    }
                }
                Cardinality::Many => TypeRef::named_nn_list(TypeRef::ID),
                _ => TypeRef::named(TypeRef::ID),
            };
        }
        FieldType::Array(inner) => {
            let inner_ref = input_field_type_ref(schema_name, field_name, inner, false);
            return if required {
                // Wrap in list with non-null
                TypeRef::named_nn_list(format!("{inner_ref}"))
            } else {
                TypeRef::named_list(format!("{inner_ref}"))
            };
        }
        FieldType::Composite(_) => TypeRef::named(JSON_SCALAR),
        _ => TypeRef::named(TypeRef::STRING),
    };

    if required {
        TypeRef::named_nn(format!("{base}"))
    } else {
        base
    }
}

/// Get the filter operators supported by a given field type.
fn filter_ops_for_type(ft: &FieldType) -> Vec<&'static str> {
    match ft {
        FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_) => {
            vec!["eq", "ne", "contains", "starts_with", "in"]
        }
        FieldType::Integer(_) | FieldType::Float(_) | FieldType::DateTime => {
            vec!["eq", "ne", "gt", "gte", "lt", "lte", "in"]
        }
        FieldType::Boolean => {
            vec!["eq", "ne"]
        }
        _ => vec!["eq", "ne"],
    }
}

/// Get the TypeRef for a filter operator value.
fn filter_value_type_ref(
    schema_name: &str,
    field_name: &str,
    ft: &FieldType,
    op: &str,
) -> TypeRef {
    if op == "in" {
        // 'in' takes a list of values
        let inner = scalar_type_for_filter(schema_name, field_name, ft);
        return TypeRef::named_list(inner);
    }
    TypeRef::named(scalar_type_for_filter(schema_name, field_name, ft))
}

/// Get the scalar type name for a filter value.
fn scalar_type_for_filter(schema_name: &str, field_name: &str, ft: &FieldType) -> String {
    match ft {
        FieldType::Text(_) | FieldType::RichText => TypeRef::STRING.to_string(),
        FieldType::Integer(_) => TypeRef::INT.to_string(),
        FieldType::Float(_) => TypeRef::FLOAT.to_string(),
        FieldType::Boolean => TypeRef::BOOLEAN.to_string(),
        FieldType::DateTime => DATETIME_SCALAR.to_string(),
        FieldType::Enum(_) => format!("{schema_name}_{field_name}"),
        _ => TypeRef::STRING.to_string(),
    }
}

/// Convert a GraphQL filter input object to a core `Filter`.
///
/// The input uses flat field-operator keys like `name_eq`, `age_gt`, `and`, `or`, `not`.
pub fn filter_input_to_filter(
    obj: &indexmap::IndexMap<async_graphql::Name, GqlValue>,
    schema: &SchemaDefinition,
) -> Result<Filter, Vec<String>> {
    let mut filters = Vec::new();
    let mut errors = Vec::new();

    for (key, value) in obj {
        let key_str = key.as_str();

        match key_str {
            "and" => {
                if let GqlValue::List(items) = value {
                    let mut sub = Vec::new();
                    for item in items {
                        if let GqlValue::Object(inner) = item {
                            match filter_input_to_filter(inner, schema) {
                                Ok(f) => sub.push(f),
                                Err(errs) => errors.extend(errs),
                            }
                        }
                    }
                    if !sub.is_empty() {
                        filters.push(Filter::and(sub));
                    }
                }
            }
            "or" => {
                if let GqlValue::List(items) = value {
                    let mut sub = Vec::new();
                    for item in items {
                        if let GqlValue::Object(inner) = item {
                            match filter_input_to_filter(inner, schema) {
                                Ok(f) => sub.push(f),
                                Err(errs) => errors.extend(errs),
                            }
                        }
                    }
                    if !sub.is_empty() {
                        filters.push(Filter::or(sub));
                    }
                }
            }
            "not" => {
                if let GqlValue::Object(inner) = value {
                    match filter_input_to_filter(inner, schema) {
                        Ok(f) => filters.push(Filter::negate(f)),
                        Err(errs) => errors.extend(errs),
                    }
                }
            }
            _ => {
                // Split on last '_' to get (field_name, operator)
                match parse_filter_field_op(key_str) {
                    Some((field_name, op)) => {
                        let json_value = gql_value_to_json(value);
                        let field_type = schema.field(field_name).map(|fd| &fd.field_type);

                        match build_leaf_filter(field_name, op, &json_value, field_type) {
                            Ok(f) => filters.push(f),
                            Err(e) => errors.push(e),
                        }
                    }
                    None => {
                        errors.push(format!("unrecognized filter key: {key_str}"));
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(match filters.len() {
        0 => Filter::and(vec![]), // empty and = match all
        1 => filters.into_iter().next().unwrap(),
        _ => Filter::and(filters),
    })
}

/// Known filter operators.
const FILTER_OPS: &[&str] = &[
    "eq",
    "ne",
    "gt",
    "gte",
    "lt",
    "lte",
    "contains",
    "starts_with",
    "in",
];

/// Parse a `field_op` key by splitting on the last known operator suffix.
fn parse_filter_field_op(key: &str) -> Option<(&str, &str)> {
    for &op in FILTER_OPS {
        let suffix = format!("_{op}");
        if let Some(field) = key.strip_suffix(&suffix) {
            if !field.is_empty() {
                return Some((field, op));
            }
        }
    }
    None
}

/// Build a leaf Filter from field name, operator, json value, and optional type hint.
fn build_leaf_filter(
    field_name: &str,
    op: &str,
    json_value: &serde_json::Value,
    field_type: Option<&FieldType>,
) -> Result<Filter, String> {
    let path = FieldPath::parse(field_name)
        .map_err(|e| format!("invalid field path '{field_name}': {e}"))?;

    match op {
        "contains" => {
            let s = json_value
                .as_str()
                .ok_or_else(|| format!("'contains' for '{field_name}' requires a string"))?;
            Ok(Filter::contains(path, s))
        }
        "starts_with" => {
            let s = json_value
                .as_str()
                .ok_or_else(|| format!("'starts_with' for '{field_name}' requires a string"))?;
            Ok(Filter::starts_with(path, s))
        }
        "in" => {
            let arr = json_value
                .as_array()
                .ok_or_else(|| format!("'in' for '{field_name}' requires an array"))?;
            let values = arr
                .iter()
                .map(|v| coerce_filter_value(v, field_type))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("field '{field_name}': {e}"))?;
            Ok(Filter::in_set(path, values))
        }
        _ => {
            let dv = coerce_filter_value(json_value, field_type)
                .map_err(|e| format!("field '{field_name}': {e}"))?;
            Ok(match op {
                "eq" => Filter::eq(path, dv),
                "ne" => Filter::ne(path, dv),
                "gt" => Filter::gt(path, dv),
                "gte" => Filter::gte(path, dv),
                "lt" => Filter::lt(path, dv),
                "lte" => Filter::lte(path, dv),
                _ => return Err(format!("unknown operator '{op}'")),
            })
        }
    }
}

/// Coerce a JSON value to a DynamicValue using an optional type hint.
fn coerce_filter_value(
    value: &serde_json::Value,
    field_type: Option<&FieldType>,
) -> Result<schema_forge_core::types::DynamicValue, String> {
    use schema_forge_core::types::DynamicValue;

    if value.is_null() {
        return Ok(DynamicValue::Null);
    }

    match field_type {
        Some(FieldType::Integer(_)) => value
            .as_i64()
            .map(DynamicValue::Integer)
            .ok_or_else(|| format!("expected integer, got {value}")),
        Some(FieldType::Float(_)) => value
            .as_f64()
            .map(DynamicValue::Float)
            .ok_or_else(|| format!("expected float, got {value}")),
        Some(FieldType::Boolean) => value
            .as_bool()
            .map(DynamicValue::Boolean)
            .ok_or_else(|| format!("expected boolean, got {value}")),
        Some(FieldType::DateTime) => {
            let s = value
                .as_str()
                .ok_or_else(|| format!("expected datetime string, got {value}"))?;
            s.parse::<chrono::DateTime<chrono::Utc>>()
                .map(DynamicValue::DateTime)
                .map_err(|e| format!("invalid datetime '{s}': {e}"))
        }
        Some(FieldType::Enum(_)) => value
            .as_str()
            .map(|s| DynamicValue::Enum(s.to_string()))
            .ok_or_else(|| format!("expected enum string, got {value}")),
        Some(FieldType::Text(_) | FieldType::RichText) => value
            .as_str()
            .map(|s| DynamicValue::Text(s.to_string()))
            .ok_or_else(|| format!("expected string, got {value}")),
        None | Some(_) => {
            // Best-effort untyped coercion
            match value {
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
                _ => Ok(DynamicValue::Text(value.to_string())),
            }
        }
    }
}

/// Convert GraphQL input object to entity fields for create/update.
///
/// Converts the GQL input to a serde_json Map, then delegates to
/// `json_to_entity_fields` which handles schema type coercion.
pub fn gql_input_to_entity_fields(
    input: &indexmap::IndexMap<async_graphql::Name, GqlValue>,
    schema: &SchemaDefinition,
) -> Result<std::collections::BTreeMap<String, schema_forge_core::types::DynamicValue>, Vec<String>>
{
    let mut json_map = serde_json::Map::new();
    for (key, value) in input {
        json_map.insert(key.to_string(), gql_value_to_json(value));
    }
    json_to_entity_fields(schema, &json_map)
}

/// Convert GraphQL input for partial update (no required field checks).
pub fn gql_input_to_partial_fields(
    input: &indexmap::IndexMap<async_graphql::Name, GqlValue>,
    schema: &SchemaDefinition,
) -> Result<std::collections::BTreeMap<String, schema_forge_core::types::DynamicValue>, Vec<String>>
{
    let mut fields = std::collections::BTreeMap::new();
    let mut errors = Vec::new();

    for (key, value) in input {
        let key_str = key.to_string();
        let json_value = gql_value_to_json(value);
        let field_def = schema.field(&key_str);

        let dv = if let Some(def) = field_def {
            coerce_filter_value(&json_value, Some(&def.field_type))
        } else {
            coerce_filter_value(&json_value, None)
        };

        match dv {
            Ok(v) => {
                fields.insert(key_str, v);
            }
            Err(e) => {
                errors.push(format!("field '{key_str}': {e}"));
            }
        }
    }

    if errors.is_empty() {
        Ok(fields)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::Name;
    use schema_forge_core::types::{
        FieldDefinition, FieldModifier, FieldName, FieldType, IntegerConstraints, SchemaId,
        SchemaName, TextConstraints,
    };

    fn test_schema() -> SchemaDefinition {
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
                    FieldType::Integer(IntegerConstraints::unconstrained()),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn parse_filter_field_op_eq() {
        assert_eq!(parse_filter_field_op("name_eq"), Some(("name", "eq")));
    }

    #[test]
    fn parse_filter_field_op_gt() {
        assert_eq!(parse_filter_field_op("age_gt"), Some(("age", "gt")));
    }

    #[test]
    fn parse_filter_field_op_contains() {
        assert_eq!(
            parse_filter_field_op("name_contains"),
            Some(("name", "contains"))
        );
    }

    #[test]
    fn parse_filter_field_op_unknown() {
        assert_eq!(parse_filter_field_op("name_foobar"), None);
    }

    #[test]
    fn filter_input_eq() {
        let schema = test_schema();
        let mut obj = indexmap::IndexMap::new();
        obj.insert(
            Name::new("name_eq"),
            GqlValue::String("Alice".into()),
        );
        let filter = filter_input_to_filter(&obj, &schema).unwrap();
        assert!(matches!(filter, Filter::Eq { .. }));
    }

    #[test]
    fn filter_input_and() {
        let schema = test_schema();
        let mut inner1 = indexmap::IndexMap::new();
        inner1.insert(
            Name::new("name_eq"),
            GqlValue::String("Alice".into()),
        );
        let mut inner2 = indexmap::IndexMap::new();
        inner2.insert(
            Name::new("age_gt"),
            GqlValue::Number(25.into()),
        );

        let mut obj = indexmap::IndexMap::new();
        obj.insert(
            Name::new("and"),
            GqlValue::List(vec![
                GqlValue::Object(inner1),
                GqlValue::Object(inner2),
            ]),
        );
        let filter = filter_input_to_filter(&obj, &schema).unwrap();
        assert!(matches!(filter, Filter::And { ref filters } if filters.len() == 2));
    }

    #[test]
    fn filter_input_not() {
        let schema = test_schema();
        let mut inner = indexmap::IndexMap::new();
        inner.insert(
            Name::new("active_eq"),
            GqlValue::Boolean(false),
        );
        let mut obj = indexmap::IndexMap::new();
        obj.insert(Name::new("not"), GqlValue::Object(inner));
        let filter = filter_input_to_filter(&obj, &schema).unwrap();
        assert!(matches!(filter, Filter::Not { .. }));
    }

    #[test]
    fn filter_input_in() {
        let schema = test_schema();
        let mut obj = indexmap::IndexMap::new();
        obj.insert(
            Name::new("age_in"),
            GqlValue::List(vec![
                GqlValue::Number(20.into()),
                GqlValue::Number(30.into()),
            ]),
        );
        let filter = filter_input_to_filter(&obj, &schema).unwrap();
        assert!(matches!(filter, Filter::In { ref values, .. } if values.len() == 2));
    }
}
