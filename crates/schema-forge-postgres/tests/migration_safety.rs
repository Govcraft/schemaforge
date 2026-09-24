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
    atomic_schema_failure_preserves_data_and_metadata(backend).await;
    // Pet precedes Owner, and both reference each other.
    let mut pet = definition("Pet", Some(("owner", "Owner")));
    pet.fields[0]
        .modifiers
        .push(schema_forge_core::types::FieldModifier::Unique);
    let owner = definition("Owner", Some(("pet", "Pet")));
    apply(backend, &pet).await;
    assert!(matches!(
        backend.finalize_schema_migrations().await,
        Err(BackendError::MigrationFailed { .. })
    ));
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

    exercise_manual_tenancy(backend).await;

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

async fn exercise_manual_tenancy(backend: &PgBackend) {
    use schema_forge_core::types::{Annotation, FieldModifier, TenantKind};
    let mut root = definition("TenantOrg", None);
    root.annotations.push(Annotation::Tenant(TenantKind::Root));
    apply(backend, &root).await;
    let tenant = backend
        .create(&Entity::new(
            root.name.clone(),
            BTreeMap::from([("label".into(), DynamicValue::Text("first".into()))]),
        ))
        .await
        .unwrap();
    let mut old = definition("TenantContact", None);
    old.fields[0].modifiers.push(FieldModifier::Unique);
    apply(backend, &old).await;
    let original = backend
        .create(&Entity::new(
            old.name.clone(),
            BTreeMap::from([("label".into(), DynamicValue::Text("retained".into()))]),
        ))
        .await
        .unwrap();
    let mut child = old.clone();
    child
        .annotations
        .push(Annotation::Tenant(TenantKind::Child {
            parent: root.name.clone(),
        }));
    assert!(DiffEngine::plan_update(&old, &child).is_err());
    assert!(backend.store_schema_metadata(&child).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_attribute WHERE attrelid='\"TenantContact\"'::regclass AND attname='_tenant'").fetch_one(backend.pool()).await.unwrap();
    assert_eq!(count, 0);

    // The documented manual path commits ownership, physical uniqueness and canonical metadata together.
    let mut tx = backend.pool().begin().await.unwrap();
    for statement in [
        "LOCK TABLE \"TenantContact\", \"TenantOrg\", \"_schema_metadata\" IN ACCESS EXCLUSIVE MODE",
        "ALTER TABLE \"TenantContact\" ADD COLUMN _tenant TEXT",
        "ALTER TABLE \"TenantContact\" DROP CONSTRAINT \"uq_TenantContact_label\"",
        "CREATE INDEX \"idx_TenantContact_tenant\" ON \"TenantContact\" (_tenant)",
        "CREATE UNIQUE INDEX \"uq_TenantContact_label\" ON \"TenantContact\" (_tenant, label)",
    ] { sqlx::query(statement).execute(&mut *tx).await.unwrap(); }
    sqlx::query("UPDATE \"TenantContact\" SET _tenant = $1")
        .bind(tenant.id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    let invalid: i64 = sqlx::query_scalar("SELECT count(*) FROM \"TenantContact\" c LEFT JOIN \"TenantOrg\" o ON c._tenant=o.id WHERE c._tenant IS NULL OR o.id IS NULL").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(invalid, 0);
    let updated = sqlx::query("UPDATE \"_schema_metadata\" SET definition=jsonb_set(definition, '{annotations}', $1) WHERE name='TenantContact'").bind(serde_json::to_value(&child.annotations).unwrap()).execute(&mut *tx).await.unwrap();
    assert_eq!(updated.rows_affected(), 1);
    tx.commit().await.unwrap();
    backend.store_schema_metadata(&child).await.unwrap();
    let stored = backend
        .load_schema_metadata(&child.name)
        .await
        .unwrap()
        .unwrap();
    assert!(DiffEngine::plan_update(&stored, &child).unwrap().is_empty());
    assert_eq!(
        backend
            .get(&child.name, &original.id)
            .await
            .unwrap()
            .fields
            .get("label"),
        Some(&DynamicValue::Text("retained".into()))
    );
    let other = backend
        .create(&Entity::new(
            root.name.clone(),
            BTreeMap::from([("label".into(), DynamicValue::Text("second".into()))]),
        ))
        .await
        .unwrap();
    backend
        .create(&Entity::new(
            child.name.clone(),
            BTreeMap::from([
                ("label".into(), DynamicValue::Text("retained".into())),
                ("_tenant".into(), DynamicValue::Text(other.id.to_string())),
            ]),
        ))
        .await
        .unwrap();
    let duplicate = Entity::new(
        child.name.clone(),
        BTreeMap::from([
            ("label".into(), DynamicValue::Text("retained".into())),
            ("_tenant".into(), DynamicValue::Text(tenant.id.to_string())),
        ]),
    );
    assert!(matches!(
        backend.create(&duplicate).await,
        Err(BackendError::UniqueViolation { .. })
    ));
    // Disabling tenant scoping cannot silently discard cross-tenant duplicate values.
    let mut tx = backend.pool().begin().await.unwrap();
    sqlx::query("DROP INDEX \"uq_TenantContact_label\"")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(sqlx::query(
        "ALTER TABLE \"TenantContact\" ADD CONSTRAINT \"uq_TenantContact_label\" UNIQUE(label)"
    )
    .execute(&mut *tx)
    .await
    .is_err());
    tx.rollback().await.unwrap();
    assert!(matches!(
        backend.create(&duplicate).await,
        Err(BackendError::UniqueViolation { .. })
    ));
}

async fn atomic_schema_failure_preserves_data_and_metadata(backend: &PgBackend) {
    let original = definition("AtomicSchema", None);
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
    // Dropping label succeeds, then strict FK finalization fails. Every earlier
    // DDL, metadata write, and revision invalidation must be rolled back.
    let mut proposed = definition("AtomicSchema", Some(("owner", "AbsentAtomicOwner")));
    proposed.id = original.id.clone();
    proposed.fields.remove(0);
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
        row
    );
    backend
        .apply_schema_change(&original.name, &[], None)
        .await
        .unwrap();
    assert!(backend
        .load_schema_metadata(&original.name)
        .await
        .unwrap()
        .is_none());
}
