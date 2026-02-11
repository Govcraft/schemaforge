use std::collections::HashMap;

use schema_forge_core::query::{FieldPath, Filter, SortOrder};
use schema_forge_core::types::{DynamicValue, FieldType, SchemaDefinition};

/// Reserved query parameter names that are not filter fields.
const RESERVED_PARAMS: &[&str] = &["limit", "offset", "sort"];

/// Supported filter operators parsed from `field__op` suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    StartsWith,
    In,
}

/// Parse a query parameter key into `(field_name, operator)`.
///
/// - `"age__gt"` → `Some(("age", Gt))`
/// - `"name"` → `Some(("name", Eq))`
/// - `"limit"` → `None` (reserved)
pub fn parse_filter_key(key: &str) -> Option<(&str, FilterOp)> {
    if RESERVED_PARAMS.contains(&key) {
        return None;
    }
    // Check for double-underscore operator suffix
    if let Some(pos) = key.rfind("__") {
        let field = &key[..pos];
        let op_str = &key[pos + 2..];
        if field.is_empty() {
            return None;
        }
        let op = match op_str {
            "eq" => FilterOp::Eq,
            "ne" => FilterOp::Ne,
            "gt" => FilterOp::Gt,
            "gte" => FilterOp::Gte,
            "lt" => FilterOp::Lt,
            "lte" => FilterOp::Lte,
            "contains" => FilterOp::Contains,
            "startswith" => FilterOp::StartsWith,
            "in" => FilterOp::In,
            _ => return None, // Unknown operator
        };
        Some((field, op))
    } else {
        Some((key, FilterOp::Eq))
    }
}

/// Coerce a raw string value into a `DynamicValue` using an optional field type hint.
///
/// When a schema field type is known, parses the string accordingly (e.g. "42" → Integer).
/// Falls back to `DynamicValue::Text` for unknown fields.
pub fn coerce_string_value(
    raw: &str,
    field_type: Option<&FieldType>,
) -> Result<DynamicValue, String> {
    match field_type {
        Some(FieldType::Integer(_)) => raw
            .parse::<i64>()
            .map(DynamicValue::Integer)
            .map_err(|_| format!("expected integer, got '{raw}'")),
        Some(FieldType::Float(_)) => raw
            .parse::<f64>()
            .map(DynamicValue::Float)
            .map_err(|_| format!("expected float, got '{raw}'")),
        Some(FieldType::Boolean) => match raw {
            "true" | "1" => Ok(DynamicValue::Boolean(true)),
            "false" | "0" => Ok(DynamicValue::Boolean(false)),
            _ => Err(format!("expected boolean (true/false), got '{raw}'")),
        },
        Some(FieldType::DateTime) => raw
            .parse::<chrono::DateTime<chrono::Utc>>()
            .map(DynamicValue::DateTime)
            .map_err(|e| format!("invalid datetime '{raw}': {e}")),
        Some(FieldType::Enum(_)) => Ok(DynamicValue::Enum(raw.to_string())),
        Some(FieldType::Text(_) | FieldType::RichText) | None => {
            Ok(DynamicValue::Text(raw.to_string()))
        }
        Some(_) => Ok(DynamicValue::Text(raw.to_string())),
    }
}

/// Parse a sort parameter string into a list of `(FieldPath, SortOrder)` pairs.
///
/// Supports two syntaxes:
/// - Prefix syntax: `"-age,name"` where `-` means descending
/// - Colon syntax: `"age:desc,name:asc"`
pub fn parse_sort_param(sort: &str) -> Result<Vec<(FieldPath, SortOrder)>, String> {
    let mut result = Vec::new();
    for part in sort.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (field, order) = if let Some(stripped) = part.strip_prefix('-') {
            (stripped, SortOrder::Descending)
        } else if let Some(stripped) = part.strip_prefix('+') {
            (stripped, SortOrder::Ascending)
        } else if let Some((field, dir)) = part.rsplit_once(':') {
            let order = match dir {
                "asc" => SortOrder::Ascending,
                "desc" => SortOrder::Descending,
                _ => return Err(format!("invalid sort direction '{dir}', expected 'asc' or 'desc'")),
            };
            (field, order)
        } else {
            (part, SortOrder::Ascending)
        };

        if field.is_empty() {
            return Err("empty field name in sort parameter".to_string());
        }
        let path = FieldPath::parse(field)
            .map_err(|e| format!("invalid sort field '{field}': {e}"))?;
        result.push((path, order));
    }
    Ok(result)
}

/// Parse all non-reserved query parameters into an optional `Filter`.
///
/// Each parameter key is parsed via `parse_filter_key`. Values are coerced using
/// schema field type hints. Multiple filter params are AND-combined.
///
/// Returns `Ok(None)` when no filter parameters are present.
pub fn parse_filter_params(
    params: &HashMap<String, String>,
    schema: &SchemaDefinition,
) -> Result<Option<Filter>, Vec<String>> {
    let mut filters = Vec::new();
    let mut errors = Vec::new();

    for (key, value) in params {
        let (field_name, op) = match parse_filter_key(key) {
            Some(pair) => pair,
            None => continue, // Reserved or unrecognized operator
        };

        let field_type = schema.field(field_name).map(|fd| &fd.field_type);
        let path = match FieldPath::parse(field_name) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("invalid field path '{field_name}': {e}"));
                continue;
            }
        };

        let filter = match op {
            FilterOp::Contains => {
                Filter::contains(path, value.as_str())
            }
            FilterOp::StartsWith => {
                Filter::starts_with(path, value.as_str())
            }
            FilterOp::In => {
                let mut values = Vec::new();
                for part in value.split(',') {
                    match coerce_string_value(part.trim(), field_type) {
                        Ok(v) => values.push(v),
                        Err(e) => errors.push(format!("field '{field_name}': {e}")),
                    }
                }
                if values.is_empty() && errors.is_empty() {
                    errors.push(format!("field '{field_name}': IN filter requires at least one value"));
                    continue;
                }
                Filter::in_set(path, values)
            }
            _ => {
                let dv = match coerce_string_value(value, field_type) {
                    Ok(v) => v,
                    Err(e) => {
                        errors.push(format!("field '{field_name}': {e}"));
                        continue;
                    }
                };
                match op {
                    FilterOp::Eq => Filter::eq(path, dv),
                    FilterOp::Ne => Filter::ne(path, dv),
                    FilterOp::Gt => Filter::gt(path, dv),
                    FilterOp::Gte => Filter::gte(path, dv),
                    FilterOp::Lt => Filter::lt(path, dv),
                    FilterOp::Lte => Filter::lte(path, dv),
                    _ => unreachable!(),
                }
            }
        };
        filters.push(filter);
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(match filters.len() {
        0 => None,
        1 => Some(filters.into_iter().next().unwrap()),
        _ => Some(Filter::and(filters)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        EnumVariants, FieldDefinition, FieldModifier, FieldName, IntegerConstraints, SchemaId,
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
                FieldDefinition::new(
                    FieldName::new("status").unwrap(),
                    FieldType::Enum(EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap()),
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    // -- parse_filter_key tests --

    #[test]
    fn parse_filter_key_plain_field() {
        assert_eq!(parse_filter_key("name"), Some(("name", FilterOp::Eq)));
    }

    #[test]
    fn parse_filter_key_eq_operator() {
        assert_eq!(parse_filter_key("name__eq"), Some(("name", FilterOp::Eq)));
    }

    #[test]
    fn parse_filter_key_gt_operator() {
        assert_eq!(parse_filter_key("age__gt"), Some(("age", FilterOp::Gt)));
    }

    #[test]
    fn parse_filter_key_gte_operator() {
        assert_eq!(parse_filter_key("age__gte"), Some(("age", FilterOp::Gte)));
    }

    #[test]
    fn parse_filter_key_lt_operator() {
        assert_eq!(parse_filter_key("age__lt"), Some(("age", FilterOp::Lt)));
    }

    #[test]
    fn parse_filter_key_lte_operator() {
        assert_eq!(parse_filter_key("age__lte"), Some(("age", FilterOp::Lte)));
    }

    #[test]
    fn parse_filter_key_ne_operator() {
        assert_eq!(parse_filter_key("status__ne"), Some(("status", FilterOp::Ne)));
    }

    #[test]
    fn parse_filter_key_contains_operator() {
        assert_eq!(
            parse_filter_key("name__contains"),
            Some(("name", FilterOp::Contains))
        );
    }

    #[test]
    fn parse_filter_key_startswith_operator() {
        assert_eq!(
            parse_filter_key("name__startswith"),
            Some(("name", FilterOp::StartsWith))
        );
    }

    #[test]
    fn parse_filter_key_in_operator() {
        assert_eq!(parse_filter_key("status__in"), Some(("status", FilterOp::In)));
    }

    #[test]
    fn parse_filter_key_reserved_limit() {
        assert_eq!(parse_filter_key("limit"), None);
    }

    #[test]
    fn parse_filter_key_reserved_offset() {
        assert_eq!(parse_filter_key("offset"), None);
    }

    #[test]
    fn parse_filter_key_reserved_sort() {
        assert_eq!(parse_filter_key("sort"), None);
    }

    #[test]
    fn parse_filter_key_unknown_operator() {
        assert_eq!(parse_filter_key("name__foobar"), None);
    }

    #[test]
    fn parse_filter_key_empty_field_prefix() {
        assert_eq!(parse_filter_key("__eq"), None);
    }

    // -- coerce_string_value tests --

    #[test]
    fn coerce_integer() {
        let result = coerce_string_value("42", Some(&FieldType::Integer(IntegerConstraints::unconstrained())));
        assert_eq!(result.unwrap(), DynamicValue::Integer(42));
    }

    #[test]
    fn coerce_integer_invalid() {
        let result = coerce_string_value("abc", Some(&FieldType::Integer(IntegerConstraints::unconstrained())));
        assert!(result.is_err());
    }

    #[test]
    fn coerce_boolean_true() {
        let result = coerce_string_value("true", Some(&FieldType::Boolean));
        assert_eq!(result.unwrap(), DynamicValue::Boolean(true));
    }

    #[test]
    fn coerce_boolean_false() {
        let result = coerce_string_value("false", Some(&FieldType::Boolean));
        assert_eq!(result.unwrap(), DynamicValue::Boolean(false));
    }

    #[test]
    fn coerce_boolean_one() {
        let result = coerce_string_value("1", Some(&FieldType::Boolean));
        assert_eq!(result.unwrap(), DynamicValue::Boolean(true));
    }

    #[test]
    fn coerce_boolean_invalid() {
        let result = coerce_string_value("maybe", Some(&FieldType::Boolean));
        assert!(result.is_err());
    }

    #[test]
    fn coerce_text() {
        let result = coerce_string_value("hello", Some(&FieldType::Text(TextConstraints::unconstrained())));
        assert_eq!(result.unwrap(), DynamicValue::Text("hello".into()));
    }

    #[test]
    fn coerce_enum() {
        let variants = EnumVariants::new(vec!["A".into(), "B".into()]).unwrap();
        let result = coerce_string_value("A", Some(&FieldType::Enum(variants)));
        assert_eq!(result.unwrap(), DynamicValue::Enum("A".into()));
    }

    #[test]
    fn coerce_no_type_hint() {
        let result = coerce_string_value("fallback", None);
        assert_eq!(result.unwrap(), DynamicValue::Text("fallback".into()));
    }

    #[test]
    fn coerce_float() {
        use schema_forge_core::types::FloatConstraints;
        let result = coerce_string_value("2.72", Some(&FieldType::Float(FloatConstraints::unconstrained())));
        match result.unwrap() {
            DynamicValue::Float(f) => assert!((f - 2.72).abs() < f64::EPSILON),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn coerce_datetime() {
        let result = coerce_string_value("2024-01-15T10:30:00Z", Some(&FieldType::DateTime));
        assert!(matches!(result.unwrap(), DynamicValue::DateTime(_)));
    }

    #[test]
    fn coerce_datetime_invalid() {
        let result = coerce_string_value("not-a-date", Some(&FieldType::DateTime));
        assert!(result.is_err());
    }

    // -- parse_sort_param tests --

    #[test]
    fn parse_sort_ascending_default() {
        let result = parse_sort_param("name").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, FieldPath::single("name"));
        assert_eq!(result[0].1, SortOrder::Ascending);
    }

    #[test]
    fn parse_sort_descending_prefix() {
        let result = parse_sort_param("-age").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, FieldPath::single("age"));
        assert_eq!(result[0].1, SortOrder::Descending);
    }

    #[test]
    fn parse_sort_ascending_prefix() {
        let result = parse_sort_param("+name").unwrap();
        assert_eq!(result[0].1, SortOrder::Ascending);
    }

    #[test]
    fn parse_sort_colon_syntax() {
        let result = parse_sort_param("age:desc").unwrap();
        assert_eq!(result[0].0, FieldPath::single("age"));
        assert_eq!(result[0].1, SortOrder::Descending);
    }

    #[test]
    fn parse_sort_colon_asc() {
        let result = parse_sort_param("name:asc").unwrap();
        assert_eq!(result[0].1, SortOrder::Ascending);
    }

    #[test]
    fn parse_sort_multiple() {
        let result = parse_sort_param("-age,name").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, FieldPath::single("age"));
        assert_eq!(result[0].1, SortOrder::Descending);
        assert_eq!(result[1].0, FieldPath::single("name"));
        assert_eq!(result[1].1, SortOrder::Ascending);
    }

    #[test]
    fn parse_sort_multiple_colon() {
        let result = parse_sort_param("age:desc,name:asc").unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_sort_invalid_direction() {
        let result = parse_sort_param("age:sideways");
        assert!(result.is_err());
    }

    #[test]
    fn parse_sort_empty_field() {
        let result = parse_sort_param("-");
        assert!(result.is_err());
    }

    #[test]
    fn parse_sort_skips_empty_parts() {
        let result = parse_sort_param("name,,age").unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_sort_dotted_path() {
        let result = parse_sort_param("-company.name").unwrap();
        assert_eq!(result[0].0, FieldPath::parse("company.name").unwrap());
        assert_eq!(result[0].1, SortOrder::Descending);
    }

    // -- parse_filter_params tests --

    #[test]
    fn parse_filter_params_no_filters() {
        let schema = test_schema();
        let params = HashMap::from([
            ("limit".to_string(), "10".to_string()),
            ("offset".to_string(), "0".to_string()),
        ]);
        let result = parse_filter_params(&params, &schema).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_filter_params_single_eq() {
        let schema = test_schema();
        let params = HashMap::from([("name".to_string(), "Alice".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(
            filter,
            Filter::Eq { ref path, ref value }
            if path == &FieldPath::single("name") && *value == DynamicValue::Text("Alice".into())
        ));
    }

    #[test]
    fn parse_filter_params_integer_coercion() {
        let schema = test_schema();
        let params = HashMap::from([("age__gt".to_string(), "25".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(
            filter,
            Filter::Gt { ref value, .. } if *value == DynamicValue::Integer(25)
        ));
    }

    #[test]
    fn parse_filter_params_boolean_coercion() {
        let schema = test_schema();
        let params = HashMap::from([("active".to_string(), "true".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(
            filter,
            Filter::Eq { ref value, .. } if *value == DynamicValue::Boolean(true)
        ));
    }

    #[test]
    fn parse_filter_params_in_operator() {
        let schema = test_schema();
        let params = HashMap::from([("status__in".to_string(), "Active,Inactive".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(filter, Filter::In { ref values, .. } if values.len() == 2));
    }

    #[test]
    fn parse_filter_params_contains() {
        let schema = test_schema();
        let params = HashMap::from([("name__contains".to_string(), "lic".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(filter, Filter::Contains { .. }));
    }

    #[test]
    fn parse_filter_params_startswith() {
        let schema = test_schema();
        let params = HashMap::from([("name__startswith".to_string(), "Al".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(filter, Filter::StartsWith { .. }));
    }

    #[test]
    fn parse_filter_params_multiple_and_combined() {
        let schema = test_schema();
        let params = HashMap::from([
            ("name".to_string(), "Alice".to_string()),
            ("age__gt".to_string(), "20".to_string()),
        ]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(filter, Filter::And { ref filters } if filters.len() == 2));
    }

    #[test]
    fn parse_filter_params_integer_coercion_error() {
        let schema = test_schema();
        let params = HashMap::from([("age__gt".to_string(), "notanumber".to_string())]);
        let result = parse_filter_params(&params, &schema);
        assert!(result.is_err());
    }

    #[test]
    fn parse_filter_params_unknown_field_as_text() {
        let schema = test_schema();
        let params = HashMap::from([("unknown_field".to_string(), "value".to_string())]);
        let result = parse_filter_params(&params, &schema).unwrap();
        let filter = result.unwrap();
        assert!(matches!(
            filter,
            Filter::Eq { ref value, .. } if *value == DynamicValue::Text("value".into())
        ));
    }

    #[test]
    fn parse_filter_params_reserved_excluded() {
        let schema = test_schema();
        let params = HashMap::from([
            ("sort".to_string(), "-age".to_string()),
            ("name".to_string(), "Alice".to_string()),
        ]);
        let result = parse_filter_params(&params, &schema).unwrap();
        // sort should be excluded, only name filter
        let filter = result.unwrap();
        assert!(matches!(filter, Filter::Eq { .. }));
    }
}
