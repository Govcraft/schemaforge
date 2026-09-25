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
    assert_eq!(backend.get(&original.name, &row.id).await.unwrap(), row);
}

#[tokio::test]
async fn fresh_metadata_reads_are_empty_without_creating_tables() {
    use schema_forge_backend::SchemaBackend;
    use schema_forge_core::types::SchemaName;
    let backend = schema_forge_surrealdb::SurrealBackend::connect_memory("fresh", "fresh")
        .await
        .unwrap();
    assert!(backend.list_schema_metadata().await.unwrap().is_empty());
    assert!(backend
        .load_schema_metadata(&SchemaName::new("Missing").unwrap())
        .await
        .unwrap()
        .is_none());
    for table in ["_schema_metadata", "Missing"] {
        let error = backend
            .client()
            .query(format!("SELECT * FROM {table};"))
            .await
            .unwrap()
            .check()
            .unwrap_err();
        assert!(matches!(
            error.not_found_details(),
            Some(surrealdb::types::NotFoundError::Table { name }) if name == table
        ));
    }
}

#[tokio::test]
async fn metadata_write_checks_statement_failures() {
    use schema_forge_backend::SchemaBackend;
    use schema_forge_core::types::*;
    let backend = schema_forge_surrealdb::SurrealBackend::connect_memory("metadata", "metadata")
        .await
        .unwrap();
    let mut schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("MetadataProbe").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("label").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();
    schema.fields[0].annotations.push(FieldAnnotation::Require {
        expr: "true".into(),
        message: "Apostrophe ' and quote \" and backslash \\ and newline\nretained".into(),
    });
    backend.store_schema_metadata(&schema).await.unwrap();
    assert_eq!(
        backend.load_schema_metadata(&schema.name).await.unwrap(),
        Some(schema.clone())
    );
    backend
        .client()
        .query("DEFINE FIELD definition ON _schema_metadata TYPE string ASSERT $value = $original;")
        .bind(("original", serde_json::to_string(&schema).unwrap()))
        .await
        .unwrap()
        .check()
        .unwrap();
    let mut changed = schema.clone();
    changed.fields[0].name = FieldName::new("changed").unwrap();
    assert!(backend.store_schema_metadata(&changed).await.is_err());
    assert_eq!(
        backend.load_schema_metadata(&schema.name).await.unwrap(),
        Some(schema)
    );
}
