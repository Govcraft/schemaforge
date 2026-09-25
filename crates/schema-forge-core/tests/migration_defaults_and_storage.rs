use schema_forge_core::{
    inverse_relations::pair_inverse_relations,
    migration::{DiffEngine, MigrationError, MigrationStep},
    types::{
        Cardinality, DefaultValue, DynamicValue, FieldDefinition, FieldModifier, FieldName,
        FieldType, SchemaDefinition, SchemaId, SchemaName, TextConstraints,
    },
};

fn text(name: &str) -> FieldDefinition {
    FieldDefinition::new(
        FieldName::new(name).unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    )
}
fn schema(name: &str, fields: Vec<FieldDefinition>) -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new(name).unwrap(),
        fields,
        vec![],
    )
    .unwrap()
}
fn required_default(mut field: FieldDefinition) -> FieldDefinition {
    field.modifiers = vec![
        FieldModifier::Required,
        FieldModifier::Default {
            value: DefaultValue::String("draft".into()),
        },
    ];
    field
}

#[test]
fn required_changes_need_usable_defaults_but_fresh_and_unchanged_schemas_do_not() {
    let old = schema("Widget", vec![text("name")]);
    let mut new = old.clone();
    let mut required = text("status");
    required.modifiers.push(FieldModifier::Required);
    new.fields.push(required.clone());
    assert!(matches!(
        DiffEngine::plan_update(&old, &new),
        Err(MigrationError::RequiredFieldWithoutDefault { .. })
    ));
    assert!(!DiffEngine::create_new(&new).is_empty());
    assert!(DiffEngine::plan_update(&new, &new).unwrap().is_empty());
    required.modifiers.push(FieldModifier::Default {
        value: DefaultValue::Integer(0),
    });
    new.fields[1] = required;
    assert!(
        DiffEngine::plan_update(&old, &new).is_err(),
        "wrong-type literal cannot backfill text"
    );
    new.fields[1] = required_default(text("status"));
    assert!(DiffEngine::plan_update(&old, &new).unwrap().is_safe());
}

#[test]
fn optional_to_required_backfills_before_installing_constraint() {
    let old = schema("Widget", vec![text("status")]);
    let new = schema("Widget", vec![required_default(text("status"))]);
    let plan = DiffEngine::plan_update(&old, &new).unwrap();
    assert!(
        matches!(&plan.steps[0], MigrationStep::BackfillRequired { field, default_value: DynamicValue::Text(value) } if field.as_str() == "status" && value == "draft")
    );
    assert!(matches!(&plan.steps[1], MigrationStep::AddRequired { .. }));
    let mut no_default = new.clone();
    no_default.fields[0].modifiers = vec![FieldModifier::Required];
    assert!(DiffEngine::plan_update(&old, &no_default).is_err());
}

fn relation(name: &str, target: &str, cardinality: Cardinality) -> FieldDefinition {
    FieldDefinition::new(
        FieldName::new(name).unwrap(),
        FieldType::Relation {
            target: SchemaName::new(target).unwrap(),
            cardinality,
        },
    )
}

#[test]
fn changes_in_child_schema_surface_parent_storage_loss_and_recreation() {
    let team = schema(
        "Team",
        vec![relation("members", "Person", Cardinality::Many)],
    );
    let person = schema("Person", vec![text("name")]);
    let mut batch = vec![team.clone(), person];
    batch[1]
        .fields
        .push(relation("backup_for", "Team", Cardinality::One));
    pair_inverse_relations(&mut batch).unwrap();
    let derived = batch[0].clone();
    let plan = DiffEngine::plan_update(&team, &derived).unwrap();
    assert!(plan.has_destructive_steps());
    assert!(
        matches!(&plan.steps[..], [MigrationStep::RemoveRelation { name }] if name.as_str() == "members")
    );
    assert!(DiffEngine::plan_update(&derived, &derived)
        .unwrap()
        .is_empty());

    batch[1].fields.pop();
    pair_inverse_relations(&mut batch).unwrap();
    assert!(
        !batch[0].fields[0].is_derived(),
        "re-pairing must clear stale metadata"
    );
    let plan = DiffEngine::plan_update(&derived, &batch[0]).unwrap();
    assert!(plan.has_destructive_steps());
    assert!(matches!(
        &plan.steps[..],
        [
            MigrationStep::RemoveRelation { .. },
            MigrationStep::AddRelation { .. }
        ]
    ));
}

#[test]
fn renamed_collection_storage_transition_does_not_rename_nonexistent_column() {
    let old = schema(
        "Team",
        vec![relation("members", "Person", Cardinality::Many)],
    );
    let mut new = old.clone();
    new.fields[0].name = FieldName::new("people").unwrap();
    new.fields[0].derived_from = Some(FieldName::new("team").unwrap());
    new.fields[0]
        .annotations
        .push(schema_forge_core::types::FieldAnnotation::RenamedFrom {
            name: FieldName::new("members").unwrap(),
        });
    let plan = DiffEngine::plan_update(&old, &new).unwrap();
    assert!(
        matches!(&plan.steps[..], [MigrationStep::RemoveRelation { name }] if name.as_str() == "members")
    );
    let mut restored = new.clone();
    restored.fields[0].name = FieldName::new("restored").unwrap();
    restored.fields[0].derived_from = None;
    restored.fields[0].annotations = vec![schema_forge_core::types::FieldAnnotation::RenamedFrom {
        name: FieldName::new("people").unwrap(),
    }];
    let plan = DiffEngine::plan_update(&new, &restored).unwrap();
    assert!(matches!(
        &plan.steps[..],
        [
            MigrationStep::RemoveRelation { .. },
            MigrationStep::AddRelation { .. }
        ]
    ));
}
