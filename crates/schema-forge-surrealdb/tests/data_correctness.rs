#[path = "../../schema-forge-backend/tests/support/data_correctness.rs"]
mod contract;

#[tokio::test]
async fn null_filters_stable_pages_and_enum_migrations() {
    let backend =
        schema_forge_surrealdb::SurrealBackend::connect_memory("correctness", "correctness")
            .await
            .unwrap();
    contract::exercise(&backend).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_clients_cannot_both_replace_the_same_snapshot() {
    use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
    use schema_forge_core::{migration::DiffEngine, types::*};
    use schema_forge_surrealdb::SurrealBackend;
    use std::collections::BTreeMap;

    let backend = SurrealBackend::connect_memory("cas", "cas").await.unwrap();
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("ConcurrentCas").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("value").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
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
    let original = DynamicValue::Text("snapshot".into());
    let replacement = DynamicValue::Text("replacement".into());
    let row = backend
        .create(&Entity::new(
            schema.name.clone(),
            BTreeMap::from([("value".into(), original.clone())]),
        ))
        .await
        .unwrap();
    // Independent backend wrappers share the database client, never an
    // application mutex. The engine must enforce the conditional write.
    let first = SurrealBackend::from_client(backend.client().clone());
    let second = SurrealBackend::from_client(backend.client().clone());
    let field = FieldName::new("value").unwrap();
    for _ in 0..256 {
        backend.update(&row).await.unwrap();
        let (replaced, cleared) = tokio::join!(
            first.update_field_if_matches(&schema.name, &row.id, &field, &original, &replacement),
            second.update_field_if_matches(
                &schema.name,
                &row.id,
                &field,
                &original,
                &DynamicValue::Null
            ),
        );
        let replaced = replaced.unwrap();
        let cleared = cleared.unwrap();
        assert_ne!(replaced, cleared, "exactly one competing write commits");
        let stored = backend.get(&schema.name, &row.id).await.unwrap();
        if replaced {
            assert_eq!(
                stored.field("value"),
                Some(&replacement),
                "a losing clear must preserve the replacement"
            );
        } else {
            assert!(matches!(
                stored.field("value"),
                None | Some(DynamicValue::Null)
            ));
        }
        backend.update(&row).await.unwrap();
        let (first_clear, second_clear) = tokio::join!(
            first.update_field_if_matches(
                &schema.name,
                &row.id,
                &field,
                &original,
                &DynamicValue::Null
            ),
            second.update_field_if_matches(
                &schema.name,
                &row.id,
                &field,
                &original,
                &DynamicValue::Null
            ),
        );
        assert_ne!(
            first_clear.unwrap(),
            second_clear.unwrap(),
            "exactly one clear commits"
        );
    }
}
