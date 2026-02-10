use schema_forge_core::migration::{
    DiffEngine, MigrationPlan, MigrationSafety, MigrationStep, ValueTransform,
};
use schema_forge_core::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_field(name: &str) -> FieldDefinition {
    FieldDefinition::new(
        FieldName::new(name).unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    )
}

fn make_field_with_type(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::new(FieldName::new(name).unwrap(), field_type)
}

fn make_schema(name: &str, fields: Vec<FieldDefinition>) -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new(name).unwrap(),
        fields,
        vec![],
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// CRM Schema Evolution Scenarios
// ---------------------------------------------------------------------------

/// Scenario: Create a brand new CRM Contact schema.
#[test]
fn scenario_create_new_crm_contact() {
    let contact = make_schema(
        "Contact",
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(512)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            ),
            make_field("phone"),
        ],
    );

    let plan = DiffEngine::create_new(&contact);

    assert_eq!(plan.len(), 1);
    assert!(plan.is_safe());
    assert_eq!(plan.schema_name.as_str(), "Contact");
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::CreateSchema { name, fields }
        if name.as_str() == "Contact" && fields.len() == 3
    ));
}

/// Scenario: Add a phone field and an indexed status enum to an existing Contact schema.
#[test]
fn scenario_add_fields_to_contact() {
    let v1 = make_schema(
        "Contact",
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(512)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            ),
        ],
    );

    let v2 = make_schema(
        "Contact",
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(512)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            ),
            make_field("phone"),
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
        ],
    );

    let plan = DiffEngine::diff(&v1, &v2);

    assert_eq!(plan.len(), 2);
    assert!(plan.is_safe());
    assert!(plan
        .steps
        .iter()
        .any(|s| matches!(s, MigrationStep::AddField { field } if field.name.as_str() == "phone")));
    assert!(plan
        .steps
        .iter()
        .any(|s| matches!(s, MigrationStep::AddField { field } if field.name.as_str() == "status")));
}

/// Scenario: Remove an obsolete field from the Contact schema.
#[test]
fn scenario_remove_field_from_contact() {
    let v1 = make_schema(
        "Contact",
        vec![
            make_field("name"),
            make_field("email"),
            make_field("fax"), // Obsolete!
        ],
    );

    let v2 = make_schema("Contact", vec![make_field("name"), make_field("email")]);

    let plan = DiffEngine::diff(&v1, &v2);

    assert_eq!(plan.len(), 1);
    assert_eq!(plan.overall_safety(), MigrationSafety::Destructive);
    assert!(plan.has_destructive_steps());
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::RemoveField { name } if name.as_str() == "fax"
    ));
}

/// Scenario: Change a field's type from Integer to Float (e.g. score becoming a decimal).
#[test]
fn scenario_change_field_type() {
    let v1 = make_schema(
        "Stats",
        vec![make_field_with_type(
            "score",
            FieldType::Integer(IntegerConstraints::with_range(0, 100).unwrap()),
        )],
    );

    let v2 = make_schema(
        "Stats",
        vec![make_field_with_type(
            "score",
            FieldType::Float(FloatConstraints::with_precision(2)),
        )],
    );

    let plan = DiffEngine::diff(&v1, &v2);

    assert_eq!(plan.len(), 1);
    assert_eq!(plan.overall_safety(), MigrationSafety::RequiresConfirmation);
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::ChangeType {
            name,
            transform: ValueTransform::IntegerToFloat,
            ..
        } if name.as_str() == "score"
    ));
}

/// Scenario: Add an index to an existing field.
#[test]
fn scenario_add_index() {
    let v1 = make_schema(
        "Contact",
        vec![FieldDefinition::new(
            FieldName::new("email").unwrap(),
            FieldType::Text(TextConstraints::with_max_length(255)),
        )],
    );

    let v2 = make_schema(
        "Contact",
        vec![FieldDefinition::with_modifiers(
            FieldName::new("email").unwrap(),
            FieldType::Text(TextConstraints::with_max_length(255)),
            vec![FieldModifier::Indexed],
        )],
    );

    let plan = DiffEngine::diff(&v1, &v2);

    assert_eq!(plan.len(), 1);
    assert!(plan.is_safe());
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::AddIndex { field } if field.as_str() == "email"
    ));
}

/// Scenario: Make a field required.
#[test]
fn scenario_make_field_required() {
    let v1 = make_schema(
        "Contact",
        vec![FieldDefinition::new(
            FieldName::new("email").unwrap(),
            FieldType::Text(TextConstraints::with_max_length(255)),
        )],
    );

    let v2 = make_schema(
        "Contact",
        vec![FieldDefinition::with_modifiers(
            FieldName::new("email").unwrap(),
            FieldType::Text(TextConstraints::with_max_length(255)),
            vec![FieldModifier::Required],
        )],
    );

    let plan = DiffEngine::diff(&v1, &v2);

    assert_eq!(plan.len(), 1);
    assert_eq!(plan.overall_safety(), MigrationSafety::RequiresConfirmation);
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::AddRequired { field } if field.as_str() == "email"
    ));
}

/// Scenario: Add a relation to another schema.
#[test]
fn scenario_add_relation() {
    let v1 = make_schema("Contact", vec![make_field("name")]);

    let v2 = make_schema(
        "Contact",
        vec![
            make_field("name"),
            FieldDefinition::new(
                FieldName::new("company").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
            FieldDefinition::new(
                FieldName::new("deals").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Deal").unwrap(),
                    cardinality: Cardinality::Many,
                },
            ),
        ],
    );

    let plan = DiffEngine::diff(&v1, &v2);

    assert_eq!(plan.len(), 2);
    assert!(plan.is_safe());
    assert!(plan.steps.iter().any(|s| matches!(
        s,
        MigrationStep::AddRelation {
            name,
            target,
            cardinality: Cardinality::One
        } if name.as_str() == "company" && target.as_str() == "Company"
    )));
    assert!(plan.steps.iter().any(|s| matches!(
        s,
        MigrationStep::AddRelation {
            name,
            target,
            cardinality: Cardinality::Many
        } if name.as_str() == "deals" && target.as_str() == "Deal"
    )));
}

/// Scenario: Full CRM evolution from v1 to v2 with multiple changes.
#[test]
fn scenario_full_crm_evolution() {
    // v1: simple contact with name, email, phone
    let v1 = make_schema(
        "Contact",
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(512)),
                vec![FieldModifier::Required],
            ),
            make_field("phone"),
            make_field("fax"),
        ],
    );

    // v2: evolved with indexed email, new status enum, company relation, removed fax
    let v2 = make_schema(
        "Contact",
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(512)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            ),
            make_field("phone"),
            FieldDefinition::with_modifiers(
                FieldName::new("status").unwrap(),
                FieldType::Enum(
                    EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap(),
                ),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("Active".into()),
                }],
            ),
            FieldDefinition::new(
                FieldName::new("company").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
        ],
    );

    let plan = DiffEngine::diff(&v1, &v2);

    // Should detect: remove fax, add status, add company, add index on email
    assert_eq!(plan.len(), 4);
    assert!(plan.has_destructive_steps()); // removing fax is destructive
    assert_eq!(plan.overall_safety(), MigrationSafety::Destructive);

    // Check the plan display is readable
    let display = plan.to_string();
    assert!(display.contains("Migration plan for 'Contact'"));
    assert!(display.contains("4 steps"));
    assert!(display.contains("destructive"));
}

/// Scenario: Serde roundtrip of a complete migration plan.
#[test]
fn scenario_migration_plan_serde_roundtrip() {
    let v1 = make_schema("Contact", vec![make_field("name")]);
    let v2 = make_schema(
        "Contact",
        vec![
            make_field("name"),
            make_field("email"),
            FieldDefinition::new(
                FieldName::new("company").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
        ],
    );

    let plan = DiffEngine::diff(&v1, &v2);
    let json = serde_json::to_string_pretty(&plan).unwrap();
    let back: MigrationPlan = serde_json::from_str(&json).unwrap();

    assert_eq!(plan, back);
    assert_eq!(plan.len(), back.len());
    assert_eq!(plan.overall_safety(), back.overall_safety());
}

/// Scenario: Diffing identical schemas produces an empty plan.
#[test]
fn scenario_no_changes() {
    let schema = make_schema(
        "Contact",
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::with_max_length(512)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            ),
        ],
    );

    let plan = DiffEngine::diff(&schema, &schema);
    assert!(plan.is_empty());
    assert!(plan.is_safe());
}

/// Scenario: Change a default value on a field.
#[test]
fn scenario_change_default_value() {
    let v1 = make_schema(
        "Contact",
        vec![FieldDefinition::with_modifiers(
            FieldName::new("priority").unwrap(),
            FieldType::Enum(
                EnumVariants::new(vec!["Low".into(), "Medium".into(), "High".into()]).unwrap(),
            ),
            vec![FieldModifier::Default {
                value: DefaultValue::String("Low".into()),
            }],
        )],
    );

    let v2 = make_schema(
        "Contact",
        vec![FieldDefinition::with_modifiers(
            FieldName::new("priority").unwrap(),
            FieldType::Enum(
                EnumVariants::new(vec!["Low".into(), "Medium".into(), "High".into()]).unwrap(),
            ),
            vec![FieldModifier::Default {
                value: DefaultValue::String("Medium".into()),
            }],
        )],
    );

    let plan = DiffEngine::diff(&v1, &v2);
    assert_eq!(plan.len(), 1);
    assert!(plan.is_safe());
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::SetDefault { field, value: DefaultValue::String(s) }
        if field.as_str() == "priority" && s == "Medium"
    ));
}

/// Scenario: Remove required modifier from a field.
#[test]
fn scenario_remove_required() {
    let v1 = make_schema(
        "Contact",
        vec![FieldDefinition::with_modifiers(
            FieldName::new("phone").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
        )],
    );

    let v2 = make_schema(
        "Contact",
        vec![FieldDefinition::new(
            FieldName::new("phone").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
    );

    let plan = DiffEngine::diff(&v1, &v2);
    assert_eq!(plan.len(), 1);
    assert!(plan.is_safe());
    assert!(matches!(
        &plan.steps[0],
        MigrationStep::RemoveRequired { field } if field.as_str() == "phone"
    ));
}
