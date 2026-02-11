use std::collections::BTreeMap;

use schema_forge_core::types::{
    Annotation, Cardinality, DynamicValue, EntityId, EnumVariants, FieldDefinition, FieldModifier,
    FieldName, FieldType, FloatConstraints, IntegerConstraints, SchemaDefinition, SchemaId,
    SchemaName, SchemaVersion, TextConstraints,
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

    // Track which schema fields we've seen
    let mut seen_fields = std::collections::HashSet::new();

    for field_def in &schema.fields {
        let name = field_def.name.as_str();
        seen_fields.insert(name.to_string());

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
                    if non_empty.is_empty() {
                        // Don't insert if all empty
                    } else {
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
                        if val.is_empty() {
                            // Skip empty non-required fields
                        } else {
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

/// Convert HTML form data to a `SchemaDefinition`.
///
/// Parses indexed field names like `field_0_name`, `field_0_type`, etc.
/// Handles non-contiguous indices (fields can be removed leaving gaps).
pub fn form_to_schema_definition(
    form_data: &[(String, String)],
    existing_id: Option<SchemaId>,
) -> Result<SchemaDefinition, Vec<String>> {
    let mut errors = Vec::new();

    // Build a lookup for form values
    let mut form_map: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in form_data {
        form_map.insert(key.clone(), value.clone());
    }

    // Extract schema-level fields
    let schema_name_str = form_map.get("schema_name").cloned().unwrap_or_default();
    let version_str = form_map.get("version").cloned().unwrap_or_default();
    let display_field_str = form_map.get("display_field").cloned().unwrap_or_default();

    // Validate schema name
    let schema_name = match SchemaName::new(&schema_name_str) {
        Ok(name) => Some(name),
        Err(e) => {
            errors.push(format!("Invalid schema name '{}': {}", schema_name_str, e));
            None
        }
    };

    // Discover field indices by scanning keys for field_N_name patterns
    let mut field_indices: Vec<usize> = Vec::new();
    for key in form_map.keys() {
        if let Some(rest) = key.strip_prefix("field_") {
            if let Some(idx_str) = rest.strip_suffix("_name") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if !field_indices.contains(&idx) {
                        field_indices.push(idx);
                    }
                }
            }
        }
    }
    field_indices.sort_unstable();

    if field_indices.is_empty() {
        errors.push("At least one field is required".to_string());
    }

    // Parse each field
    let mut fields = Vec::new();
    let mut seen_field_names = Vec::new();
    for idx in &field_indices {
        let prefix = format!("field_{idx}_");
        let field_name_str = form_map
            .get(&format!("{prefix}name"))
            .cloned()
            .unwrap_or_default();
        let field_type_str = form_map
            .get(&format!("{prefix}type"))
            .cloned()
            .unwrap_or_default();

        if field_name_str.is_empty() {
            errors.push(format!("Field {} has an empty name", idx));
            continue;
        }

        // Validate field name
        let field_name = match FieldName::new(&field_name_str) {
            Ok(name) => name,
            Err(e) => {
                errors.push(format!("Invalid field name '{}': {}", field_name_str, e));
                continue;
            }
        };

        // Check for duplicates
        if seen_field_names.contains(&field_name_str) {
            errors.push(format!("Duplicate field name '{}'", field_name_str));
            continue;
        }
        seen_field_names.push(field_name_str.clone());

        // Build FieldType from type string + constraints
        let field_type = match field_type_str.as_str() {
            "text" => {
                let max_len = form_map
                    .get(&format!("{prefix}text_max_length"))
                    .and_then(|v| {
                        if v.is_empty() {
                            None
                        } else {
                            v.parse::<u32>().ok()
                        }
                    });
                match max_len {
                    Some(max) => FieldType::Text(TextConstraints::with_max_length(max)),
                    None => FieldType::Text(TextConstraints::unconstrained()),
                }
            }
            "richtext" => FieldType::RichText,
            "integer" => {
                let min = form_map.get(&format!("{prefix}integer_min")).and_then(|v| {
                    if v.is_empty() {
                        None
                    } else {
                        v.parse::<i64>().ok()
                    }
                });
                let max = form_map.get(&format!("{prefix}integer_max")).and_then(|v| {
                    if v.is_empty() {
                        None
                    } else {
                        v.parse::<i64>().ok()
                    }
                });
                match (min, max) {
                    (Some(mn), Some(mx)) => match IntegerConstraints::with_range(mn, mx) {
                        Ok(c) => FieldType::Integer(c),
                        Err(e) => {
                            errors.push(format!("Field '{}': {}", field_name_str, e));
                            continue;
                        }
                    },
                    (Some(mn), None) => FieldType::Integer(IntegerConstraints::with_min(mn)),
                    (None, Some(mx)) => FieldType::Integer(IntegerConstraints::with_max(mx)),
                    (None, None) => FieldType::Integer(IntegerConstraints::unconstrained()),
                }
            }
            "float" => {
                let precision = form_map
                    .get(&format!("{prefix}float_precision"))
                    .and_then(|v| {
                        if v.is_empty() {
                            None
                        } else {
                            v.parse::<u32>().ok()
                        }
                    });
                match precision {
                    Some(p) => FieldType::Float(FloatConstraints::with_precision(p)),
                    None => FieldType::Float(FloatConstraints::unconstrained()),
                }
            }
            "boolean" => FieldType::Boolean,
            "datetime" => FieldType::DateTime,
            "enum" => {
                let variants_str = form_map
                    .get(&format!("{prefix}enum_variants"))
                    .cloned()
                    .unwrap_or_default();
                let variant_list: Vec<String> = variants_str
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                match EnumVariants::new(variant_list) {
                    Ok(v) => FieldType::Enum(v),
                    Err(e) => {
                        errors.push(format!("Field '{}': {}", field_name_str, e));
                        continue;
                    }
                }
            }
            "json" => FieldType::Json,
            "relation" => {
                let target_str = form_map
                    .get(&format!("{prefix}relation_target"))
                    .cloned()
                    .unwrap_or_default();
                let cardinality_str = form_map
                    .get(&format!("{prefix}relation_cardinality"))
                    .cloned()
                    .unwrap_or_else(|| "one".to_string());
                let target = match SchemaName::new(&target_str) {
                    Ok(t) => t,
                    Err(e) => {
                        errors.push(format!(
                            "Field '{}': invalid relation target '{}': {}",
                            field_name_str, target_str, e
                        ));
                        continue;
                    }
                };
                let cardinality = match cardinality_str.as_str() {
                    "many" => Cardinality::Many,
                    _ => Cardinality::One,
                };
                FieldType::Relation {
                    target,
                    cardinality,
                }
            }
            other => {
                errors.push(format!(
                    "Field '{}': unknown type '{}'",
                    field_name_str, other
                ));
                continue;
            }
        };

        // Build modifiers
        let mut modifiers = Vec::new();
        if form_map
            .get(&format!("{prefix}required"))
            .is_some_and(|v| v == "true" || v == "on")
        {
            modifiers.push(FieldModifier::Required);
        }
        if form_map
            .get(&format!("{prefix}indexed"))
            .is_some_and(|v| v == "true" || v == "on")
        {
            modifiers.push(FieldModifier::Indexed);
        }

        let field_def = if modifiers.is_empty() {
            FieldDefinition::new(field_name, field_type)
        } else {
            FieldDefinition::with_modifiers(field_name, field_type, modifiers)
        };
        fields.push(field_def);
    }

    // Build annotations
    let mut annotations = Vec::new();
    if !version_str.is_empty() {
        match version_str.parse::<u32>() {
            Ok(v) => match SchemaVersion::new(v) {
                Ok(sv) => annotations.push(Annotation::Version { version: sv }),
                Err(e) => errors.push(format!("Invalid version '{}': {}", version_str, e)),
            },
            Err(_) => errors.push(format!("Version '{}' is not a valid number", version_str)),
        }
    }
    if !display_field_str.is_empty() {
        match FieldName::new(&display_field_str) {
            Ok(fname) => annotations.push(Annotation::Display { field: fname }),
            Err(e) => errors.push(format!(
                "Invalid display field '{}': {}",
                display_field_str, e
            )),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let id = existing_id.unwrap_or_default();
    let schema_name = schema_name.expect("validated above");

    SchemaDefinition::new(id, schema_name, fields, annotations).map_err(|e| vec![e.to_string()])
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
            // Try parsing as RFC3339 first, then datetime-local format
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
    fn float_field() {
        let schema = make_schema(vec![make_field(
            "price",
            FieldType::Float(FloatConstraints::with_precision(2)),
        )]);
        let form = vec![("price".to_string(), "9.99".to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(result.get("price"), Some(&DynamicValue::Float(9.99)));
    }

    #[test]
    fn boolean_checkbox_present() {
        let schema = make_schema(vec![make_field("active", FieldType::Boolean)]);
        // Hidden + checkbox pattern
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
        let form = vec![]; // checkbox not submitted
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(result.get("active"), Some(&DynamicValue::Boolean(false)));
    }

    #[test]
    fn datetime_field() {
        let schema = make_schema(vec![make_field("created_at", FieldType::DateTime)]);
        let form = vec![("created_at".to_string(), "2024-01-15T10:30".to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert!(matches!(
            result.get("created_at"),
            Some(DynamicValue::DateTime(_))
        ));
    }

    #[test]
    fn enum_field() {
        let variants = EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap();
        let schema = make_schema(vec![make_field("status", FieldType::Enum(variants))]);
        let form = vec![("status".to_string(), "Active".to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert_eq!(
            result.get("status"),
            Some(&DynamicValue::Enum("Active".into()))
        );
    }

    #[test]
    fn json_field() {
        let schema = make_schema(vec![make_field("metadata", FieldType::Json)]);
        let form = vec![("metadata".to_string(), r#"{"key": "value"}"#.to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert!(matches!(
            result.get("metadata"),
            Some(DynamicValue::Json(_))
        ));
    }

    #[test]
    fn json_field_invalid() {
        let schema = make_schema(vec![make_field("metadata", FieldType::Json)]);
        let form = vec![("metadata".to_string(), "not json".to_string())];
        let result = form_to_entity_fields(&schema, &form);
        assert!(result.is_err());
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

    #[test]
    fn relation_one_field() {
        let entity_id = EntityId::new();
        let schema = make_schema(vec![make_field(
            "company",
            FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::One,
            },
        )]);
        let form = vec![("company".to_string(), entity_id.as_str().to_string())];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        assert!(matches!(result.get("company"), Some(DynamicValue::Ref(_))));
    }

    #[test]
    fn relation_many_field() {
        let id1 = EntityId::new();
        let id2 = EntityId::new();
        let schema = make_schema(vec![make_field(
            "tags",
            FieldType::Relation {
                target: SchemaName::new("Tag").unwrap(),
                cardinality: Cardinality::Many,
            },
        )]);
        let form = vec![
            ("tags".to_string(), id1.as_str().to_string()),
            ("tags".to_string(), id2.as_str().to_string()),
        ];
        let result = form_to_entity_fields(&schema, &form).unwrap();
        if let Some(DynamicValue::RefArray(ids)) = result.get("tags") {
            assert_eq!(ids.len(), 2);
        } else {
            panic!("expected RefArray");
        }
    }

    // -----------------------------------------------------------------------
    // form_to_schema_definition tests
    // -----------------------------------------------------------------------

    #[test]
    fn schema_form_basic_text_field() {
        let form = vec![
            ("schema_name".into(), "Contact".into()),
            ("field_0_name".into(), "name".into()),
            ("field_0_type".into(), "text".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        assert_eq!(result.name.as_str(), "Contact");
        assert_eq!(result.fields.len(), 1);
        assert_eq!(result.fields[0].name.as_str(), "name");
        assert!(matches!(result.fields[0].field_type, FieldType::Text(_)));
    }

    #[test]
    fn schema_form_all_field_types() {
        let form = vec![
            ("schema_name".into(), "Multi".into()),
            ("field_0_name".into(), "title".into()),
            ("field_0_type".into(), "text".into()),
            ("field_0_text_max_length".into(), "200".into()),
            ("field_1_name".into(), "count".into()),
            ("field_1_type".into(), "integer".into()),
            ("field_1_integer_min".into(), "0".into()),
            ("field_1_integer_max".into(), "100".into()),
            ("field_2_name".into(), "price".into()),
            ("field_2_type".into(), "float".into()),
            ("field_2_float_precision".into(), "2".into()),
            ("field_3_name".into(), "active".into()),
            ("field_3_type".into(), "boolean".into()),
            ("field_4_name".into(), "created_at".into()),
            ("field_4_type".into(), "datetime".into()),
            ("field_5_name".into(), "metadata".into()),
            ("field_5_type".into(), "json".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        assert_eq!(result.fields.len(), 6);

        assert!(matches!(
            result.fields[0].field_type,
            FieldType::Text(TextConstraints {
                max_length: Some(200)
            })
        ));
        assert!(matches!(
            result.fields[1].field_type,
            FieldType::Integer(IntegerConstraints {
                min: Some(0),
                max: Some(100)
            })
        ));
        assert!(matches!(
            result.fields[2].field_type,
            FieldType::Float(FloatConstraints { precision: Some(2) })
        ));
        assert!(matches!(result.fields[3].field_type, FieldType::Boolean));
        assert!(matches!(result.fields[4].field_type, FieldType::DateTime));
        assert!(matches!(result.fields[5].field_type, FieldType::Json));
    }

    #[test]
    fn schema_form_text_constraints() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "bio".into()),
            ("field_0_type".into(), "text".into()),
            ("field_0_text_max_length".into(), "500".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        if let FieldType::Text(c) = &result.fields[0].field_type {
            assert_eq!(c.max_length, Some(500));
        } else {
            panic!("expected Text");
        }
    }

    #[test]
    fn schema_form_integer_constraints() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "age".into()),
            ("field_0_type".into(), "integer".into()),
            ("field_0_integer_min".into(), "0".into()),
            ("field_0_integer_max".into(), "150".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        if let FieldType::Integer(c) = &result.fields[0].field_type {
            assert_eq!(c.min, Some(0));
            assert_eq!(c.max, Some(150));
        } else {
            panic!("expected Integer");
        }
    }

    #[test]
    fn schema_form_float_constraints() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "price".into()),
            ("field_0_type".into(), "float".into()),
            ("field_0_float_precision".into(), "2".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        if let FieldType::Float(c) = &result.fields[0].field_type {
            assert_eq!(c.precision, Some(2));
        } else {
            panic!("expected Float");
        }
    }

    #[test]
    fn schema_form_modifiers() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "email".into()),
            ("field_0_type".into(), "text".into()),
            ("field_0_required".into(), "true".into()),
            ("field_0_indexed".into(), "true".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        assert!(result.fields[0].is_required());
        assert!(result.fields[0].is_indexed());
    }

    #[test]
    fn schema_form_annotations() {
        let form = vec![
            ("schema_name".into(), "Contact".into()),
            ("version".into(), "2".into()),
            ("display_field".into(), "name".into()),
            ("field_0_name".into(), "name".into()),
            ("field_0_type".into(), "text".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        let has_version = result
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::Version { version } if version.get() == 2));
        let has_display = result
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::Display { field } if field.as_str() == "name"));
        assert!(has_version);
        assert!(has_display);
    }

    #[test]
    fn schema_form_invalid_schema_name() {
        let form = vec![
            ("schema_name".into(), "not-valid".into()),
            ("field_0_name".into(), "name".into()),
            ("field_0_type".into(), "text".into()),
        ];
        let result = form_to_schema_definition(&form, None);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.contains("schema name")));
    }

    #[test]
    fn schema_form_invalid_field_name() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "NotSnakeCase".into()),
            ("field_0_type".into(), "text".into()),
        ];
        let result = form_to_schema_definition(&form, None);
        assert!(result.is_err());
    }

    #[test]
    fn schema_form_empty_fields() {
        let form = vec![("schema_name".into(), "Test".into())];
        let result = form_to_schema_definition(&form, None);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.contains("field is required")));
    }

    #[test]
    fn schema_form_duplicate_names() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "name".into()),
            ("field_0_type".into(), "text".into()),
            ("field_1_name".into(), "name".into()),
            ("field_1_type".into(), "text".into()),
        ];
        let result = form_to_schema_definition(&form, None);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.contains("Duplicate")));
    }

    #[test]
    fn schema_form_enum_field() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "status".into()),
            ("field_0_type".into(), "enum".into()),
            (
                "field_0_enum_variants".into(),
                "Active\nInactive\nPending".into(),
            ),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        if let FieldType::Enum(v) = &result.fields[0].field_type {
            assert_eq!(v.as_slice(), &["Active", "Inactive", "Pending"]);
        } else {
            panic!("expected Enum");
        }
    }

    #[test]
    fn schema_form_relation_field() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "company".into()),
            ("field_0_type".into(), "relation".into()),
            ("field_0_relation_target".into(), "Company".into()),
            ("field_0_relation_cardinality".into(), "one".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        if let FieldType::Relation {
            target,
            cardinality,
        } = &result.fields[0].field_type
        {
            assert_eq!(target.as_str(), "Company");
            assert!(matches!(cardinality, Cardinality::One));
        } else {
            panic!("expected Relation");
        }
    }

    #[test]
    fn schema_form_relation_invalid_target() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "company".into()),
            ("field_0_type".into(), "relation".into()),
            ("field_0_relation_target".into(), "not_valid".into()),
            ("field_0_relation_cardinality".into(), "one".into()),
        ];
        let result = form_to_schema_definition(&form, None);
        assert!(result.is_err());
    }

    #[test]
    fn schema_form_integer_min_greater_than_max() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "value".into()),
            ("field_0_type".into(), "integer".into()),
            ("field_0_integer_min".into(), "100".into()),
            ("field_0_integer_max".into(), "10".into()),
        ];
        let result = form_to_schema_definition(&form, None);
        assert!(result.is_err());
    }

    #[test]
    fn schema_form_preserves_existing_id() {
        let existing_id = SchemaId::new();
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "name".into()),
            ("field_0_type".into(), "text".into()),
        ];
        let result = form_to_schema_definition(&form, Some(existing_id.clone())).unwrap();
        assert_eq!(result.id, existing_id);
    }

    #[test]
    fn schema_form_non_contiguous_indices() {
        let form = vec![
            ("schema_name".into(), "Test".into()),
            ("field_0_name".into(), "first".into()),
            ("field_0_type".into(), "text".into()),
            ("field_5_name".into(), "second".into()),
            ("field_5_type".into(), "integer".into()),
            ("field_10_name".into(), "third".into()),
            ("field_10_type".into(), "boolean".into()),
        ];
        let result = form_to_schema_definition(&form, None).unwrap();
        assert_eq!(result.fields.len(), 3);
        assert_eq!(result.fields[0].name.as_str(), "first");
        assert_eq!(result.fields[1].name.as_str(), "second");
        assert_eq!(result.fields[2].name.as_str(), "third");
    }
}
