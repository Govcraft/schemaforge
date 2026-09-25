//! Field modifier migrations retain complete definitions and remain atomic.
use std::collections::BTreeMap;

use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::{DiffEngine, MigrationStep},
    types::*,
};
use schema_forge_surrealdb::SurrealBackend;

async fn fixture(namespace: &str) -> (SurrealBackend, SchemaDefinition, Entity) {
    let backend = SurrealBackend::connect_memory(namespace, namespace)
        .await
        .unwrap();
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Widget").unwrap(),
        vec![FieldDefinition::with_modifiers(
            FieldName::new("status").unwrap(),
            FieldType::Text(TextConstraints::with_max_length(8)),
            vec![FieldModifier::Indexed],
        )],
        vec![],
    )
    .unwrap();
    backend
        .apply_schema_change(
            &schema.name,
            &DiffEngine::create_new(&schema).steps,
            Some(&schema),
        )
        .await
        .unwrap();
    let row = Entity::new(schema.name.clone(), BTreeMap::new());
    backend.create(&row).await.unwrap();
    (backend, schema, row)
}

#[tokio::test]
async fn backfill_and_required_default_changes_preserve_type_and_constraints() {
    let (backend, mut schema, row) = fixture("modifiers").await;
    let field = schema.fields[0].name.clone();
    let steps = [
        MigrationStep::BackfillRequired {
            field: field.clone(),
            default_value: DynamicValue::Text("draft".into()),
        },
        MigrationStep::AddRequired {
            field: field.clone(),
        },
        MigrationStep::SetDefault {
            field: field.clone(),
            value: DefaultValue::String("draft".into()),
        },
    ];
    schema.fields[0].modifiers.extend([
        FieldModifier::Required,
        FieldModifier::Default {
            value: DefaultValue::String("draft".into()),
        },
    ]);
    backend
        .apply_schema_change(&schema.name, &steps, Some(&schema))
        .await
        .unwrap();
    assert_eq!(
        backend
            .get(&schema.name, &row.id)
            .await
            .unwrap()
            .field("status"),
        Some(&DynamicValue::Text("draft".into()))
    );
    let defaulted = Entity::new(schema.name.clone(), BTreeMap::new());
    backend.create(&defaulted).await.unwrap();
    assert_eq!(
        backend
            .get(&schema.name, &defaulted.id)
            .await
            .unwrap()
            .field("status"),
        Some(&DynamicValue::Text("draft".into()))
    );
    for value in [
        DynamicValue::Text("longer-than-eight".into()),
        DynamicValue::Integer(7),
    ] {
        let invalid = Entity::new(
            schema.name.clone(),
            BTreeMap::from([("status".into(), value)]),
        );
        assert!(backend.create(&invalid).await.is_err());
    }
    schema.fields[0]
        .modifiers
        .retain(|modifier| !matches!(modifier, FieldModifier::Default { .. }));
    backend
        .apply_schema_change(
            &schema.name,
            &[MigrationStep::RemoveDefault {
                field: field.clone(),
            }],
            Some(&schema),
        )
        .await
        .unwrap();
    assert!(backend
        .create(&Entity::new(schema.name.clone(), BTreeMap::new()))
        .await
        .is_err());
    schema.fields[0]
        .modifiers
        .retain(|modifier| !matches!(modifier, FieldModifier::Required));
    backend
        .apply_schema_change(
            &schema.name,
            &[MigrationStep::RemoveRequired { field }],
            Some(&schema),
        )
        .await
        .unwrap();
    backend
        .create(&Entity::new(schema.name.clone(), BTreeMap::new()))
        .await
        .unwrap();
    assert!(backend
        .create(&Entity::new(
            schema.name.clone(),
            BTreeMap::from([("status".into(), DynamicValue::Text("still-too-long".into()))])
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn rename_then_required_default_changes_use_the_renamed_definition() {
    let (backend, mut schema, row) = fixture("renamedmodifiers").await;
    let old_name = schema.fields[0].name.clone();
    let new_name = FieldName::new("phase").unwrap();
    let steps = [
        MigrationStep::RenameField {
            old_name,
            new_name: new_name.clone(),
        },
        MigrationStep::BackfillRequired {
            field: new_name.clone(),
            default_value: DynamicValue::Text("ready".into()),
        },
        MigrationStep::AddRequired {
            field: new_name.clone(),
        },
        MigrationStep::SetDefault {
            field: new_name.clone(),
            value: DefaultValue::String("ready".into()),
        },
    ];
    // apply_migration intentionally exercises intermediate step compilation;
    // schema metadata remains old until the caller commits the final definition.
    backend.apply_migration(&schema.name, &steps).await.unwrap();
    schema.fields[0].name = new_name;
    schema.fields[0].modifiers.extend([
        FieldModifier::Required,
        FieldModifier::Default {
            value: DefaultValue::String("ready".into()),
        },
    ]);
    backend.store_schema_metadata(&schema).await.unwrap();
    assert_eq!(
        backend
            .get(&schema.name, &row.id)
            .await
            .unwrap()
            .field("phase"),
        Some(&DynamicValue::Text("ready".into()))
    );
    assert!(backend
        .create(&Entity::new(
            schema.name.clone(),
            BTreeMap::from([(
                "phase".into(),
                DynamicValue::Text("longer-than-eight".into())
            )])
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn failed_required_change_rolls_back_prior_backfill() {
    let (backend, schema, row) = fixture("modifierrollback").await;
    let mut proposed = schema.clone();
    proposed.fields[0].modifiers.extend([
        FieldModifier::Required,
        FieldModifier::Default {
            value: DefaultValue::String("draft".into()),
        },
    ]);
    let steps = [
        MigrationStep::BackfillRequired {
            field: schema.fields[0].name.clone(),
            default_value: DynamicValue::Text("draft".into()),
        },
        MigrationStep::AddRequired {
            field: schema.fields[0].name.clone(),
        },
        MigrationStep::SetDefault {
            field: schema.fields[0].name.clone(),
            value: DefaultValue::String("draft".into()),
        },
    ];
    backend
        .client()
        .query("DEFINE FIELD definition ON _schema_metadata TYPE string ASSERT $value = $original;")
        .bind(("original", serde_json::to_string(&schema).unwrap()))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(backend
        .apply_schema_change(&schema.name, &steps, Some(&proposed))
        .await
        .is_err());
    assert_eq!(
        backend
            .get(&schema.name, &row.id)
            .await
            .unwrap()
            .field("status"),
        None
    );
    assert_eq!(
        backend.load_schema_metadata(&schema.name).await.unwrap(),
        Some(schema)
    );
}

#[tokio::test]
async fn relation_cleanup_tolerates_a_derived_field_without_storage() {
    let (backend, schema, _) = fixture("relationcleanup").await;
    let name = FieldName::new("members").unwrap();
    backend
        .apply_migration(
            &schema.name,
            &[
                MigrationStep::RemoveRelation { name: name.clone() },
                MigrationStep::AddRelation {
                    name,
                    target: schema.name.clone(),
                    cardinality: Cardinality::Many,
                },
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn new_fields_with_defaults_fill_existing_rows() {
    let (backend, mut schema, row) = fixture("addeddefaults").await;
    let mut steps = Vec::new();
    for (name, required) in [("required_status", true), ("optional_status", false)] {
        let mut modifiers = vec![FieldModifier::Default {
            value: DefaultValue::String("draft".into()),
        }];
        if required {
            modifiers.push(FieldModifier::Required);
        }
        let field = FieldDefinition::with_modifiers(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::with_max_length(8)),
            modifiers,
        );
        steps.push(MigrationStep::AddField {
            field: field.clone(),
        });
        schema.fields.push(field);
    }
    backend
        .apply_schema_change(&schema.name, &steps, Some(&schema))
        .await
        .unwrap();
    let loaded = backend.get(&schema.name, &row.id).await.unwrap();
    for name in ["required_status", "optional_status"] {
        assert_eq!(
            loaded.field(name),
            Some(&DynamicValue::Text("draft".into()))
        );
    }
}
