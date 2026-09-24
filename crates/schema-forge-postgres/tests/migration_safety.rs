//! Live checks for migration parity, legacy repair, and relation conflicts.
use schema_forge_backend::{BackendError, Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::DiffEngine,
    types::{
        Cardinality, DynamicValue, EntityId, FieldDefinition, FieldName, FieldType,
        SchemaDefinition, SchemaId, SchemaName, TextConstraints,
    },
};
use schema_forge_postgres::PgBackend;
use sqlx::postgres::PgPoolOptions;
use std::{collections::BTreeMap, sync::Arc};

fn definition(name: &str, relation: Option<(&str, &str)>) -> SchemaDefinition {
    let mut fields = vec![FieldDefinition::new(
        FieldName::new("label").unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    )];
    if let Some((field, target)) = relation {
        fields.push(FieldDefinition::new(
            FieldName::new(field).unwrap(),
            FieldType::Relation {
                target: SchemaName::new(target).unwrap(),
                cardinality: Cardinality::One,
            },
        ));
    }
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new(name).unwrap(),
        fields,
        vec![],
    )
    .unwrap()
}

async fn apply(backend: &PgBackend, definition: &SchemaDefinition) {
    let old = backend
        .load_schema_metadata(&definition.name)
        .await
        .unwrap();
    let plan = old.as_ref().map_or_else(
        || DiffEngine::create_new(definition),
        |old| DiffEngine::diff(old, definition),
    );
    backend
        .apply_migration(&definition.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(definition).await.unwrap();
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn relations_have_consistent_integrity_and_legacy_constraints_are_repaired() {
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let namespace = EntityId::new("migrationtest").to_string();
    sqlx::query(&format!("CREATE SCHEMA \"{namespace}\""))
        .execute(&admin)
        .await
        .unwrap();
    let scope = namespace.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let scope = scope.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path', $1, false)")
                    .bind(scope)
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let backend = Arc::new(PgBackend::from_pool(pool.clone()).await.unwrap());
    let result = tokio::spawn(async move { exercise(&backend).await }).await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    result.unwrap();
}

async fn exercise(backend: &PgBackend) {
    // Pet precedes Owner, and both reference each other.
    let mut pet = definition("Pet", Some(("owner", "Owner")));
    pet.fields[0]
        .modifiers
        .push(schema_forge_core::types::FieldModifier::Unique);
    let owner = definition("Owner", Some(("pet", "Pet")));
    apply(backend, &pet).await;
    apply(backend, &owner).await;
    let owner_row = backend
        .create(&Entity::new(
            owner.name.clone(),
            BTreeMap::from([("label".into(), DynamicValue::Text("owner".into()))]),
        ))
        .await
        .unwrap();
    let pet_row = backend
        .create(&Entity::new(
            pet.name.clone(),
            BTreeMap::from([
                ("owner".into(), DynamicValue::Ref(owner_row.id.clone())),
                ("label".into(), DynamicValue::Text("kept".into())),
            ]),
        ))
        .await
        .unwrap();
    let deletion = backend.delete(&owner.name, &owner_row.id).await;
    assert!(
        matches!(deletion, Err(BackendError::ForeignKeyViolation { .. })),
        "unexpected deletion outcome: {deletion:?}"
    );
    let missing = Entity::new(
        pet.name.clone(),
        BTreeMap::from([("owner".into(), DynamicValue::Ref(EntityId::new("owner")))]),
    );
    assert!(matches!(
        backend.create(&missing).await,
        Err(BackendError::ForeignKeyViolation { .. })
    ));
    assert!(matches!(
        backend
            .update(&Entity::with_id(
                pet_row.id.clone(),
                pet.name.clone(),
                BTreeMap::new()
            ))
            .await,
        Err(BackendError::ValidationFailed { .. })
    ));

    // A later relation gets exactly the same integrity behavior.
    let mut changed = pet.clone();
    changed.fields.push(FieldDefinition::new(
        FieldName::new("vet").unwrap(),
        FieldType::Relation {
            target: owner.name.clone(),
            cardinality: Cardinality::One,
        },
    ));
    apply(backend, &changed).await;
    let patch = Entity::with_id(
        pet_row.id.clone(),
        pet.name.clone(),
        BTreeMap::from([("vet".into(), DynamicValue::Ref(EntityId::new("owner")))]),
    );
    assert!(matches!(
        backend.update(&patch).await,
        Err(BackendError::ForeignKeyViolation { .. })
    ));

    // Simulate legacy table without its original FK, then reconnect.
    sqlx::query("ALTER TABLE \"Pet\" DROP CONSTRAINT \"Pet_owner_fkey\"")
        .execute(backend.pool())
        .await
        .unwrap();
    let repaired = PgBackend::from_pool(backend.pool().clone()).await.unwrap();
    repaired.finalize_schema_migrations().await.unwrap();
    assert!(matches!(
        repaired.delete(&owner.name, &owner_row.id).await,
        Err(BackendError::ForeignKeyViolation { .. })
    ));
    // Reconciliation is idempotent.
    apply(&repaired, &changed).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_constraint WHERE conrelid='\"Pet\"'::regclass AND contype='f'",
    )
    .fetch_one(backend.pool())
    .await
    .unwrap();
    assert_eq!(count, 2);

    // Renaming a populated unique field preserves both its data and named constraints.
    let mut renamed = changed.clone();
    renamed.fields[0].name = FieldName::new("title").unwrap();
    renamed.fields[0]
        .annotations
        .push(schema_forge_core::types::FieldAnnotation::RenamedFrom {
            name: FieldName::new("label").unwrap(),
        });
    apply(&repaired, &renamed).await;
    let value: String = sqlx::query_scalar("SELECT title FROM \"Pet\" WHERE id = $1")
        .bind(pet_row.id.as_str())
        .fetch_one(backend.pool())
        .await
        .unwrap();
    assert_eq!(value, "kept");
    apply(&repaired, &renamed).await;
    let mut without_unique = renamed.clone();
    without_unique.fields[0].modifiers.clear();
    apply(&repaired, &without_unique).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_constraint WHERE conrelid='\"Pet\"'::regclass AND contype='u'",
    )
    .fetch_one(backend.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);

    // A renamed enum must also rename its CHECK, so widening removes the old restriction.
    let mut enum_schema = definition("EnumProbe", None);
    enum_schema.fields[0].field_type =
        FieldType::Enum(schema_forge_core::types::EnumVariants::new(vec!["old".into()]).unwrap());
    apply(backend, &enum_schema).await;
    let enum_row = backend
        .create(&Entity::new(
            enum_schema.name.clone(),
            BTreeMap::from([("label".into(), DynamicValue::Enum("old".into()))]),
        ))
        .await
        .unwrap();
    let mut renamed_enum = enum_schema.clone();
    renamed_enum.fields[0].name = FieldName::new("status").unwrap();
    renamed_enum.fields[0].annotations.push(
        schema_forge_core::types::FieldAnnotation::RenamedFrom {
            name: FieldName::new("label").unwrap(),
        },
    );
    apply(backend, &renamed_enum).await;
    renamed_enum.fields[0].field_type = FieldType::Enum(
        schema_forge_core::types::EnumVariants::new(vec!["old".into(), "new".into()]).unwrap(),
    );
    apply(backend, &renamed_enum).await;
    let updated = backend
        .update(&Entity::with_id(
            enum_row.id,
            renamed_enum.name,
            BTreeMap::from([("status".into(), DynamicValue::Enum("new".into()))]),
        ))
        .await
        .unwrap();
    assert_eq!(
        updated.fields.get("status"),
        Some(&DynamicValue::Enum("new".into()))
    );

    // Legacy orphan values must fail repair visibly rather than weakening integrity.
    sqlx::query("ALTER TABLE \"Pet\" DROP CONSTRAINT \"Pet_owner_fkey\"")
        .execute(backend.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE \"Pet\" SET owner = 'orphan'")
        .execute(backend.pool())
        .await
        .unwrap();
    assert!(matches!(
        backend.finalize_schema_migrations().await,
        Err(BackendError::MigrationFailed { .. })
    ));
}
