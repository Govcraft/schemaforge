#[path = "../../schema-forge-backend/tests/support/migration_renames.rs"]
mod contract;

#[tokio::test]
async fn declared_renames_preserve_values_and_schema_constraints() {
    let backend = schema_forge_surrealdb::SurrealBackend::connect_memory("renames", "renames")
        .await
        .unwrap();
    contract::exercise(&backend).await;
}

#[tokio::test]
async fn metadata_failure_rolls_back_destructive_schema_steps() {
    use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
    use schema_forge_core::{migration::DiffEngine, types::*};
    use std::collections::BTreeMap;
    let backend = schema_forge_surrealdb::SurrealBackend::connect_memory("atomic", "atomic")
        .await
        .unwrap();
    let original = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("AtomicSchema").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("label").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();
    backend
        .apply_schema_change(
            &original.name,
            &DiffEngine::create_new(&original).steps,
            Some(&original),
        )
        .await
        .unwrap();
    let row = Entity {
        id: EntityId::new("atomic"),
        schema: original.name.clone(),
        fields: BTreeMap::from([("label".into(), DynamicValue::Text("retained".into()))]),
    };
    backend.create(&row).await.unwrap();
    let mut proposed = original.clone();
    proposed.fields[0].name = FieldName::new("replacement").unwrap();
    let old_json = serde_json::to_string(&original).unwrap();
    backend.client().query("DEFINE FIELD definition ON _schema_metadata TYPE string ASSERT $value = $original_definition;")
        .bind(("original_definition", old_json)).await.unwrap().check().unwrap();
    let plan = DiffEngine::plan_update(&original, &proposed).unwrap();
    assert!(backend
        .apply_schema_change(&original.name, &plan.steps, Some(&proposed))
        .await
        .is_err());
    assert_eq!(
        backend.load_schema_metadata(&original.name).await.unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        backend.get(&original.name, &row.id).await.unwrap(),
        Some(row)
    );
}
