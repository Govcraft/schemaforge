use schema_forge_core::types::*;

/// Build a complete CRM Contact schema programmatically, serialize to JSON,
/// deserialize back, and assert equality.
#[test]
fn full_crm_contact_schema_serde_roundtrip() {
    let schema = build_crm_contact_schema();

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&schema).unwrap();

    // Deserialize back
    let back: SchemaDefinition = serde_json::from_str(&json).unwrap();

    // Assert equality
    assert_eq!(schema, back);

    // Verify structural properties
    assert_eq!(back.name.as_str(), "Contact");
    assert_eq!(back.fields.len(), 9);
    assert_eq!(back.annotations.len(), 2);

    // Verify individual fields
    let first_name = back.field("first_name").expect("first_name field");
    assert!(first_name.is_required());
    assert!(matches!(&first_name.field_type, FieldType::Text(_)));

    let email = back.field("email").expect("email field");
    assert!(email.is_required());
    assert!(email.is_indexed());

    let status = back.field("status").expect("status field");
    assert!(matches!(&status.field_type, FieldType::Enum(_)));

    let company = back.field("company").expect("company field");
    assert!(matches!(
        &company.field_type,
        FieldType::Relation {
            cardinality: Cardinality::One,
            ..
        }
    ));

    let tags = back.field("tags").expect("tags field");
    assert!(matches!(&tags.field_type, FieldType::Array(_)));

    let address = back.field("address").expect("address field");
    assert!(matches!(&address.field_type, FieldType::Composite(_)));

    let metadata = back.field("metadata").expect("metadata field");
    assert!(matches!(&metadata.field_type, FieldType::Json));

    let active = back.field("active").expect("active field");
    assert!(matches!(&active.field_type, FieldType::Boolean));
}

#[test]
fn crm_schema_display_output() {
    let schema = build_crm_contact_schema();
    let display = schema.to_string();
    assert!(display.contains("@version(1)"));
    assert!(display.contains("schema Contact {"));
    assert!(display.contains("first_name: Text @required"));
    assert!(display.contains("email: Text @required @indexed"));
}

#[test]
fn crm_schema_dynamic_values_roundtrip() {
    use std::collections::BTreeMap;

    // Build a sample record for the CRM Contact schema using DynamicValue
    let mut address = BTreeMap::new();
    address.insert(
        "street".to_string(),
        DynamicValue::Text("123 Main St".into()),
    );
    address.insert("city".to_string(), DynamicValue::Text("Springfield".into()));
    address.insert("zip".to_string(), DynamicValue::Text("62701".into()));

    let record = vec![
        ("first_name", DynamicValue::Text("Jane".into())),
        ("last_name", DynamicValue::Text("Doe".into())),
        ("email", DynamicValue::Text("jane@example.com".into())),
        ("status", DynamicValue::Enum("Active".into())),
        ("company", DynamicValue::Ref(EntityId::new())),
        (
            "tags",
            DynamicValue::Array(vec![
                DynamicValue::Text("vip".into()),
                DynamicValue::Text("partner".into()),
            ]),
        ),
        ("address", DynamicValue::Composite(address)),
        (
            "metadata",
            DynamicValue::Json(serde_json::json!({"source": "import"})),
        ),
        ("active", DynamicValue::Boolean(true)),
    ];

    // Serialize each field value and roundtrip
    for (name, value) in &record {
        let json = serde_json::to_string(value)
            .unwrap_or_else(|e| panic!("Failed to serialize {name}: {e}"));
        let back: DynamicValue = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("Failed to deserialize {name}: {e}"));
        assert_eq!(value, &back, "Roundtrip failed for field '{name}'");
    }
}

fn build_crm_contact_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Contact").unwrap(),
        vec![
            // first_name: Text @required
            FieldDefinition::with_modifiers(
                FieldName::new("first_name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(100)),
                vec![FieldModifier::Required],
            ),
            // last_name: Text @required
            FieldDefinition::with_modifiers(
                FieldName::new("last_name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(100)),
                vec![FieldModifier::Required],
            ),
            // email: Text @required @indexed
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            ),
            // status: Enum(Active, Inactive, Lead) @default("Lead")
            FieldDefinition::with_modifiers(
                FieldName::new("status").unwrap(),
                FieldType::Enum(
                    EnumVariants::new(vec!["Active".into(), "Inactive".into(), "Lead".into()])
                        .unwrap(),
                ),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("Lead".into()),
                }],
            ),
            // company: Relation(Company, One)
            FieldDefinition::new(
                FieldName::new("company").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
            // tags: Array<Text>
            FieldDefinition::new(
                FieldName::new("tags").unwrap(),
                FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            ),
            // address: Composite { street, city, zip }
            FieldDefinition::new(
                FieldName::new("address").unwrap(),
                FieldType::Composite(vec![
                    FieldDefinition::new(
                        FieldName::new("street").unwrap(),
                        FieldType::Text(TextConstraints::unconstrained()),
                    ),
                    FieldDefinition::new(
                        FieldName::new("city").unwrap(),
                        FieldType::Text(TextConstraints::unconstrained()),
                    ),
                    FieldDefinition::new(
                        FieldName::new("zip").unwrap(),
                        FieldType::Text(TextConstraints::with_max_length(10)),
                    ),
                ]),
            ),
            // metadata: Json
            FieldDefinition::new(FieldName::new("metadata").unwrap(), FieldType::Json),
            // active: Boolean @default(true)
            FieldDefinition::with_modifiers(
                FieldName::new("active").unwrap(),
                FieldType::Boolean,
                vec![FieldModifier::Default {
                    value: DefaultValue::Boolean(true),
                }],
            ),
        ],
        vec![
            Annotation::Version {
                version: SchemaVersion::new(1).unwrap(),
            },
            Annotation::Display {
                field: FieldName::new("email").unwrap(),
            },
        ],
    )
    .unwrap()
}
