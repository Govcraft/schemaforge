use std::collections::BTreeMap;

use schema_forge_core::types::{
    Cardinality, DynamicValue, EntityId, FieldType, SchemaDefinition,
};

/// Convert HTML form data to entity fields using schema type hints.
///
/// HTML forms submit all values as strings. This parses them back
/// using the schema's field types.
pub fn form_to_entity_fields(
    schema: &SchemaDefinition,
    form_data: &[(String, String)],
) -> Result<BTreeMap<String, DynamicValue>, Vec<String>> {
    let mut fields = BTreeMap::new();
    let mut errors = Vec::new();

    // Group form values by field name (multi-value for checkboxes, selects)
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, value) in form_data {
        // Handle composite dot-notation: "address.street" -> separate later
        grouped.entry(key.clone()).or_default().push(value.clone());
    }

    for field_def in &schema.fields {
        let name = field_def.name.as_str();

        match &field_def.field_type {
            FieldType::Boolean => {
                // Checkbox: absent = false, "true" = true, "false" = false
                let val = grouped
                    .get(name)
                    .and_then(|vals| vals.last())
                    .map(|v| v == "true" || v == "on")
                    .unwrap_or(false);
                fields.insert(name.to_string(), DynamicValue::Boolean(val));
            }
            FieldType::Relation {
                cardinality: Cardinality::Many,
                ..
            } => {
                if let Some(vals) = grouped.get(name) {
                    let non_empty: Vec<&str> = vals
                        .iter()
                        .map(|s| s.as_str())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !non_empty.is_empty() {
                        let mut ids = Vec::new();
                        for v in &non_empty {
                            match EntityId::parse(v) {
                                Ok(id) => ids.push(id),
                                Err(e) => errors.push(format!(
                                    "field '{name}': invalid entity reference '{v}': {e}"
                                )),
                            }
                        }
                        if errors.is_empty() || !ids.is_empty() {
                            fields.insert(name.to_string(), DynamicValue::RefArray(ids));
                        }
                    }
                }
            }
            FieldType::Composite(sub_fields) => {
                // Look for dot-notation keys: "field_name.sub_field"
                let prefix = format!("{name}.");
                let mut composite = BTreeMap::new();
                for (key, vals) in &grouped {
                    if let Some(sub_key) = key.strip_prefix(&prefix) {
                        if let Some(val) = vals.last() {
                            if !val.is_empty() {
                                // Find the sub-field type
                                let sub_def =
                                    sub_fields.iter().find(|f| f.name.as_str() == sub_key);
                                match parse_single_value(
                                    sub_key,
                                    val,
                                    sub_def.map(|d| &d.field_type),
                                ) {
                                    Ok(dv) => {
                                        composite.insert(sub_key.to_string(), dv);
                                    }
                                    Err(e) => errors.push(format!("field '{name}.{sub_key}': {e}")),
                                }
                            }
                        }
                    }
                }
                if !composite.is_empty() {
                    fields.insert(name.to_string(), DynamicValue::Composite(composite));
                }
            }
            other => {
                if let Some(vals) = grouped.get(name) {
                    if let Some(val) = vals.last() {
                        if !val.is_empty() {
                            match parse_single_value(name, val, Some(other)) {
                                Ok(dv) => {
                                    fields.insert(name.to_string(), dv);
                                }
                                Err(e) => errors.push(e),
                            }
                        }
                    }
                }
            }
        }
    }

    // Check required fields
    for field_def in &schema.fields {
        if field_def.is_required() && !fields.contains_key(field_def.name.as_str()) {
            errors.push(format!(
                "required field '{}' is missing",
                field_def.name.as_str()
            ));
        }
    }

    if errors.is_empty() {
        Ok(fields)
    } else {
        Err(errors)
    }
}

/// Parse a single form value using the field type hint.
fn parse_single_value(
    name: &str,
    value: &str,
    field_type: Option<&FieldType>,
) -> Result<DynamicValue, String> {
    match field_type {
        Some(FieldType::Text(_) | FieldType::RichText) => Ok(DynamicValue::Text(value.to_string())),
        Some(FieldType::Integer(_)) => value
            .parse::<i64>()
            .map(DynamicValue::Integer)
            .map_err(|_| format!("field '{name}': expected integer, got '{value}'")),
        Some(FieldType::Float(_)) => value
            .parse::<f64>()
            .map(DynamicValue::Float)
            .map_err(|_| format!("field '{name}': expected number, got '{value}'")),
        Some(FieldType::Boolean) => Ok(DynamicValue::Boolean(value == "true" || value == "on")),
        Some(FieldType::DateTime) => {
            // HTML datetime-local: "2024-01-15T10:30"
            if let Ok(dt) = value.parse::<chrono::DateTime<chrono::Utc>>() {
                Ok(DynamicValue::DateTime(dt))
            } else if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
            {
                Ok(DynamicValue::DateTime(naive.and_utc()))
            } else if let Ok(naive) =
                chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
            {
                Ok(DynamicValue::DateTime(naive.and_utc()))
            } else {
                Err(format!("field '{name}': invalid datetime format '{value}'"))
            }
        }
        Some(FieldType::Enum(_)) => Ok(DynamicValue::Enum(value.to_string())),
        Some(FieldType::Json) => serde_json::from_str(value)
            .map(DynamicValue::Json)
            .map_err(|e| format!("field '{name}': invalid JSON: {e}")),
        Some(FieldType::Relation {
            cardinality: Cardinality::One,
            ..
        }) => EntityId::parse(value)
            .map(DynamicValue::Ref)
            .map_err(|e| format!("field '{name}': invalid entity reference: {e}")),
        _ => Ok(DynamicValue::Text(value.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::*;

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::new(FieldName::new(name).unwrap(), ft)
    }

    fn make_required_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::with_modifiers(
            FieldName::new(name).unwrap(),
            ft,
            vec![FieldModifier::Required],
        )
    }

    fn make_schema(fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("TestSchema").unwrap(),
            fields,
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn basic_text_field() {
        let schema = make_schema(vec![make_field(
            "name",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let form = vec![("name".to_string(), "Alice".to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(
            result.get("name"),
            Some(&DynamicValue::Text("Alice".into()))
        );
    }

    #[test]
    fn integer_field() {
        let schema = make_schema(vec![make_field(
            "age",
            FieldType::Integer(IntegerConstraints::unconstrained()),
        )]);
        let form = vec![("age".to_string(), "30".to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(result.get("age"), Some(&DynamicValue::Integer(30)));
    }

    #[test]
    fn integer_field_invalid() {
        let schema = make_schema(vec![make_field(
            "age",
            FieldType::Integer(IntegerConstraints::unconstrained()),
        )]);
        let form = vec![("age".to_string(), "not-a-number".to_string())];
        let result = form_to_entity_fields(&schema, &form);
        assert!(result.is_err());
    }

    #[test]
    fn boolean_checkbox_present() {
        let schema = make_schema(vec![make_field("active", FieldType::Boolean)]);
        let form = vec![
            ("active".to_string(), "false".to_string()),
            ("active".to_string(), "true".to_string()),
        ];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(result.get("active"), Some(&DynamicValue::Boolean(true)));
    }

    #[test]
    fn boolean_checkbox_absent() {
        let schema = make_schema(vec![make_field("active", FieldType::Boolean)]);
        let form = vec![];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(result.get("active"), Some(&DynamicValue::Boolean(false)));
    }

    #[test]
    fn required_field_missing() {
        let schema = make_schema(vec![make_required_field(
            "name",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let form = vec![];
        let result = form_to_entity_fields(&schema, &form);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("required") && e.contains("name")));
    }

    #[test]
    fn empty_value_skipped_for_optional() {
        let schema = make_schema(vec![make_field(
            "notes",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let form = vec![("notes".to_string(), String::new())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert!(!result.contains_key("notes"));
    }

    #[test]
    fn composite_dot_notation() {
        let sub_fields = vec![
            make_field("street", FieldType::Text(TextConstraints::unconstrained())),
            make_field("city", FieldType::Text(TextConstraints::unconstrained())),
        ];
        let schema = make_schema(vec![make_field(
            "address",
            FieldType::Composite(sub_fields),
        )]);
        let form = vec![
            ("address.street".to_string(), "123 Main St".to_string()),
            ("address.city".to_string(), "Springfield".to_string()),
        ];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        let address = result.get("address").unwrap();
        if let DynamicValue::Composite(map) = address {
            assert_eq!(
                map.get("street"),
                Some(&DynamicValue::Text("123 Main St".into()))
            );
            assert_eq!(
                map.get("city"),
                Some(&DynamicValue::Text("Springfield".into()))
            );
        } else {
            panic!("expected Composite");
        }
    }
}
