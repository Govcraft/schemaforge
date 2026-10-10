#![cfg(feature = "test-support")]

#[path = "../../schema-forge-backend/tests/support/data_correctness.rs"]
mod contract;

#[tokio::test]
async fn null_filters_stable_pages_and_enum_migrations() {
    let backend = schema_forge_surrealdb::test_support::connect("correctness", "correctness")
        .await
        .unwrap();
    contract::exercise(&backend).await;
}

#[tokio::test]
async fn prefix_filters_execute_with_escaped_values_and_filtered_counts() {
    use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
    use schema_forge_core::{
        migration::DiffEngine,
        query::{FieldPath, Filter, Query, SortOrder},
        types::*,
    };
    use std::collections::{BTreeMap, BTreeSet};

    let backend = schema_forge_surrealdb::test_support::connect("prefix", "prefix")
        .await
        .unwrap();
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("PrefixQuery").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("title").unwrap(),
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
    for title in [
        "Ada",
        "Adam",
        "Bea",
        "O'Neil",
        "O'Neal",
        "OBrien",
        r"Path\one",
        r"Path\two",
        "Path/three",
    ] {
        backend
            .create(&Entity::new(
                schema.name.clone(),
                BTreeMap::from([("title".into(), DynamicValue::Text(title.into()))]),
            ))
            .await
            .unwrap();
    }
    for (prefix, expected) in [
        ("A", vec!["Ada", "Adam"]),
        ("O'", vec!["O'Neal", "O'Neil"]),
        (r"Path\", vec![r"Path\one", r"Path\two"]),
        ("missing", vec![]),
    ] {
        let result = backend
            .query(
                &Query::new(schema.id.clone())
                    .with_filter(Filter::starts_with(FieldPath::single("title"), prefix)),
            )
            .await
            .unwrap();
        assert_eq!(result.total_count, Some(expected.len()), "{prefix}");
        let titles: BTreeSet<_> = result
            .entities
            .iter()
            .map(|entity| match entity.field("title") {
                Some(DynamicValue::Text(title)) => title.as_str(),
                other => panic!("unexpected title: {other:?}"),
            })
            .collect();
        assert_eq!(titles, expected.into_iter().collect(), "{prefix}");
    }
    let page = backend
        .query(
            &Query::new(schema.id)
                .with_filter(Filter::starts_with(FieldPath::single("title"), "A"))
                .with_sort(FieldPath::single("title"), SortOrder::Ascending)
                .with_limit(1)
                .with_offset(1),
        )
        .await
        .unwrap();
    assert_eq!(page.total_count, Some(2));
    assert_eq!(page.entities.len(), 1);
    assert_eq!(
        page.entities[0].field("title"),
        Some(&DynamicValue::Text("Adam".into()))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_clients_cannot_both_replace_the_same_snapshot() {
    use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
    use schema_forge_core::{migration::DiffEngine, types::*};
    use schema_forge_surrealdb::SurrealBackend;
    use std::collections::BTreeMap;

    let backend = schema_forge_surrealdb::test_support::connect("cas", "cas")
        .await
        .unwrap();
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
