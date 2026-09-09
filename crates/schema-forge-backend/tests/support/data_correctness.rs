//! One behavior contract exercised by every storage backend.
use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::{DiffEngine, MigrationSafety, MigrationStep, ValueTransform},
    query::{FieldPath, Filter, Query, SortOrder},
    types::{
        DynamicValue, EnumVariants, FieldDefinition, FieldName, FieldType, SchemaDefinition,
        SchemaId, SchemaName, TextConstraints,
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub async fn exercise(backend: &(impl EntityStore + SchemaBackend)) {
    enum_narrowing_with_changed_default_preserves_retained_rows(backend).await;
    atomic_attachment_clear_preserves_concurrent_changes(backend).await;
    required_enum_narrowing_preserves_rows(backend).await;
    let mut schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Correctness").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("status").unwrap(),
                FieldType::Enum(EnumVariants::new(vec!["pending".into(), "live".into()]).unwrap()),
            ),
        ],
        vec![],
    )
    .unwrap();
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let mut ids = Vec::new();
    for index in 0..9 {
        let mut fields = BTreeMap::from([(
            "status".into(),
            DynamicValue::Enum(if index % 2 == 0 { "pending" } else { "live" }.into()),
        )]);
        match index % 3 {
            0 => {}
            1 => {
                fields.insert("email".into(), DynamicValue::Null);
            }
            _ => {
                fields.insert("email".into(), DynamicValue::Text("a@example.org".into()));
            }
        }
        let entity = Entity::new(schema.name.clone(), fields);
        ids.push(entity.id.clone());
        backend.create(&entity).await.unwrap();
    }
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for (value, equal_count) in [
        (DynamicValue::Null, 6),
        (DynamicValue::Text("a@example.org".into()), 3),
    ] {
        for (filter, count) in [
            (
                Filter::eq(FieldPath::single("email"), value.clone()),
                equal_count,
            ),
            (
                Filter::ne(FieldPath::single("email"), value),
                9 - equal_count,
            ),
        ] {
            let result = backend
                .query(&Query::new(schema.id.clone()).with_filter(filter))
                .await
                .unwrap();
            assert_eq!(result.entities.len(), count);
            assert_eq!(result.total_count, Some(count));
        }
    }
    for sort in [
        vec![],
        vec![(FieldPath::single("status"), SortOrder::Ascending)],
    ] {
        let mut walked = Vec::new();
        for offset in [0, 2, 4, 6, 8] {
            let mut query = Query::new(schema.id.clone())
                .with_limit(2)
                .with_offset(offset);
            query.sort = sort.clone();
            walked.extend(
                backend
                    .query(&query)
                    .await
                    .unwrap()
                    .entities
                    .into_iter()
                    .map(|e| e.id.to_string()),
            );
        }
        assert_eq!(walked.len(), 9);
        assert_eq!(walked.iter().collect::<BTreeSet<_>>().len(), 9);
        if sort.is_empty() {
            assert_eq!(
                walked,
                ids.iter().map(ToString::to_string).collect::<Vec<_>>()
            );
        }
    }
    let descending = backend
        .query(
            &Query::new(schema.id.clone())
                .with_sort(FieldPath::single("id"), SortOrder::Descending)
                .with_limit(3),
        )
        .await
        .unwrap();
    assert_eq!(
        descending
            .entities
            .iter()
            .map(|e| &e.id)
            .collect::<Vec<_>>(),
        ids.iter().rev().take(3).collect::<Vec<_>>()
    );
    let range = backend
        .query(
            &Query::new(schema.id.clone())
                .with_filter(Filter::gt(
                    FieldPath::single("id"),
                    DynamicValue::Text(ids[4].to_string()),
                ))
                .with_limit(10),
        )
        .await
        .unwrap();
    assert_eq!(
        range.entities.iter().map(|e| &e.id).collect::<Vec<_>>(),
        ids.iter().skip(5).collect::<Vec<_>>()
    );

    let original = schema.clone();
    schema.fields[1].field_type = FieldType::Enum(
        EnumVariants::new(vec!["live".into(), "blocked".into(), "pending".into()]).unwrap(),
    );
    let widening = DiffEngine::diff(&original, &schema);
    assert!(widening
        .steps
        .iter()
        .all(|step| step.safety() == MigrationSafety::Safe));
    backend
        .apply_migration(&schema.name, &widening.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    for id in &ids {
        assert!(matches!(
            backend.get(&schema.name, id).await.unwrap().field("status"),
            Some(DynamicValue::Enum(_) | DynamicValue::Text(_))
        ));
    }
    let widened = schema.clone();
    schema.fields[1].field_type =
        FieldType::Enum(EnumVariants::new(vec!["live".into(), "blocked".into()]).unwrap());
    let narrowing = DiffEngine::diff(&widened, &schema);
    assert!(narrowing.steps.iter().any(|step| matches!(step, MigrationStep::ChangeType { transform: ValueTransform::NullRemovedEnumVariants { variants }, .. } if variants == &["pending"])));
    backend
        .apply_migration(&schema.name, &narrowing.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let live = backend
        .query(&Query::new(schema.id.clone()).with_filter(Filter::eq(
            FieldPath::single("status"),
            DynamicValue::Enum("live".into()),
        )))
        .await
        .unwrap();
    assert_eq!(live.entities.len(), 4);
    let removed = backend
        .query(
            &Query::new(schema.id)
                .with_filter(Filter::eq(FieldPath::single("status"), DynamicValue::Null)),
        )
        .await
        .unwrap();
    assert_eq!(removed.entities.len(), 5);
}

pub async fn required_enum_narrowing_preserves_rows(backend: &(impl EntityStore + SchemaBackend)) {
    let mut schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("RequiredCorrectness").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("status").unwrap(),
            FieldType::Enum(EnumVariants::new(vec!["pending".into(), "live".into()]).unwrap()),
        )],
        vec![],
    )
    .unwrap();
    schema.fields[0]
        .modifiers
        .push(schema_forge_core::types::FieldModifier::Required);
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let entity = Entity::new(
        schema.name.clone(),
        BTreeMap::from([("status".into(), DynamicValue::Enum("pending".into()))]),
    );
    backend.create(&entity).await.unwrap();
    let old = schema.clone();
    schema.fields[0].field_type = FieldType::Enum(EnumVariants::new(vec!["live".into()]).unwrap());
    assert!(backend
        .apply_migration(&schema.name, &DiffEngine::diff(&old, &schema).steps)
        .await
        .is_err());
    let stored = backend.get(&schema.name, &entity.id).await.unwrap();
    assert!(
        matches!(stored.field("status"), Some(DynamicValue::Enum(value) | DynamicValue::Text(value)) if value == "pending")
    );
}

pub async fn atomic_attachment_clear_preserves_concurrent_changes(
    backend: &(impl EntityStore + SchemaBackend),
) {
    use schema_forge_core::types::{FileAccess, FileConstraints, MimePattern};
    let field = FieldName::new("attachment").unwrap();
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("AttachmentCas").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                field.clone(),
                FieldType::File(FileConstraints {
                    bucket: "documents".into(),
                    max_size_bytes: 1024,
                    mime_allowlist: vec![MimePattern::Exact("text/plain".into())],
                    access: FileAccess::Presigned,
                }),
            ),
        ],
        vec![],
    )
    .unwrap();
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let attachment = |key: &str| {
        DynamicValue::Json(
            serde_json::json!({"key": key, "mime":"text/plain", "size":1,"status":"available","created_at":"2026-01-01T00:00:00Z","uploaded_at":"2026-01-01T00:00:00Z","checksum":null}),
        )
    };
    let entity = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("old".into())),
            ("attachment".into(), attachment("object-a")),
        ]),
    );
    backend.create(&entity).await.unwrap();
    let mut snapshot = backend.get(&schema.name, &entity.id).await.unwrap();
    let expected = snapshot.field("attachment").unwrap().clone();
    snapshot.fields.insert(
        "title".into(),
        DynamicValue::Text("concurrent title".into()),
    );
    backend.update(&snapshot).await.unwrap();
    assert!(backend
        .update_field_if_matches(
            &schema.name,
            &entity.id,
            &field,
            &expected,
            &DynamicValue::Null
        )
        .await
        .unwrap());
    let cleared = backend.get(&schema.name, &entity.id).await.unwrap();
    assert_eq!(
        cleared.field("title"),
        Some(&DynamicValue::Text("concurrent title".into()))
    );
    assert!(matches!(
        cleared.field("attachment"),
        None | Some(DynamicValue::Null)
    ));
    assert!(!backend
        .update_field_if_matches(
            &schema.name,
            &entity.id,
            &field,
            &expected,
            &DynamicValue::Null
        )
        .await
        .unwrap());
    snapshot
        .fields
        .insert("attachment".into(), attachment("object-b"));
    backend.update(&snapshot).await.unwrap();
    assert!(!backend
        .update_field_if_matches(
            &schema.name,
            &entity.id,
            &field,
            &expected,
            &DynamicValue::Null
        )
        .await
        .unwrap());
    let replacement = backend.get(&schema.name, &entity.id).await.unwrap();
    let expected_b = replacement.field("attachment").unwrap();
    let (first, second) = tokio::join!(
        backend.update_field_if_matches(
            &schema.name,
            &entity.id,
            &field,
            expected_b,
            &DynamicValue::Null
        ),
        backend.update_field_if_matches(
            &schema.name,
            &entity.id,
            &field,
            expected_b,
            &DynamicValue::Null
        )
    );
    assert_ne!(
        first.unwrap(),
        second.unwrap(),
        "exactly one concurrent clear wins"
    );
    assert!(
        !backend
            .update_field_if_matches(
                &schema.name,
                &entity.id,
                &field,
                expected_b,
                &attachment("stale scanner result")
            )
            .await
            .unwrap(),
        "stale completion cannot resurrect a cleared attachment"
    );
    assert!(backend
        .update_field_if_matches(
            &schema.name,
            &entity.id,
            &field,
            &DynamicValue::Null,
            &attachment("fresh upload")
        )
        .await
        .unwrap());
    let empty = Entity::new(
        schema.name.clone(),
        BTreeMap::from([("title".into(), DynamicValue::Text("initially empty".into()))]),
    );
    backend.create(&empty).await.unwrap();
    assert!(backend
        .update_field_if_matches(
            &schema.name,
            &empty.id,
            &field,
            &DynamicValue::Null,
            &attachment("first upload")
        )
        .await
        .unwrap());
    assert!(!backend
        .update_field_if_matches(
            &schema.name,
            &schema_forge_core::types::EntityId::new("missing"),
            &field,
            &DynamicValue::Null,
            &attachment("must not create record")
        )
        .await
        .unwrap());
    assert!(!backend
        .update_field_if_matches(
            &schema.name,
            &schema_forge_core::types::EntityId::new("missing"),
            &field,
            expected_b,
            &DynamicValue::Null
        )
        .await
        .unwrap());
}

pub async fn enum_narrowing_with_changed_default_preserves_retained_rows(
    backend: &(impl EntityStore + SchemaBackend),
) {
    use schema_forge_core::types::{DefaultValue, FieldModifier};
    let mut schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("EnumDefault").unwrap(),
        vec![FieldDefinition::with_modifiers(
            FieldName::new("status").unwrap(),
            FieldType::Enum(EnumVariants::new(vec!["pending".into(), "live".into()]).unwrap()),
            vec![FieldModifier::Default {
                value: DefaultValue::String("pending".into()),
            }],
        )],
        vec![],
    )
    .unwrap();
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let pending = Entity::new(
        schema.name.clone(),
        BTreeMap::from([("status".into(), DynamicValue::Enum("pending".into()))]),
    );
    let live = Entity::new(
        schema.name.clone(),
        BTreeMap::from([("status".into(), DynamicValue::Enum("live".into()))]),
    );
    backend.create(&pending).await.unwrap();
    backend.create(&live).await.unwrap();
    let old = schema.clone();
    schema.fields[0].field_type =
        FieldType::Enum(EnumVariants::new(vec!["live".into(), "blocked".into()]).unwrap());
    schema.fields[0].modifiers = vec![FieldModifier::Default {
        value: DefaultValue::String("live".into()),
    }];
    backend
        .apply_migration(&schema.name, &DiffEngine::diff(&old, &schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let retained = backend.get(&schema.name, &live.id).await.unwrap();
    assert!(
        matches!(retained.field("status"), Some(DynamicValue::Enum(value) | DynamicValue::Text(value)) if value == "live")
    );
    let removed = backend.get(&schema.name, &pending.id).await.unwrap();
    assert!(
        matches!(removed.field("status"), None | Some(DynamicValue::Null)),
        "removed variant must be null, not reintroduced by a default: {removed:?}"
    );
}
