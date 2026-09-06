//! Opt-in live PostgreSQL concurrency checks, isolated in a disposable namespace.

use std::{collections::BTreeMap, sync::Arc};

use schema_forge_backend::{
    create_intent::{
        CreateFingerprint, CreateIntentError, CreateIntentId, CreateIntentRequest,
        CreateIntentScope,
    },
    Entity, EntityStore, SchemaBackend,
};
use schema_forge_core::{
    migration::DiffEngine,
    types::{
        DynamicValue, EntityId, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId,
        SchemaName, TextConstraints,
    },
};
use schema_forge_postgres::PgBackend;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Barrier;

fn definition() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Note").unwrap(),
        ["name", "detail"]
            .map(|name| {
                FieldDefinition::new(
                    FieldName::new(name).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                )
            })
            .to_vec(),
        vec![],
    )
    .unwrap()
}

fn patch(entity: &Entity, name: &str) -> Entity {
    Entity::with_id(
        entity.id.clone(),
        entity.schema.clone(),
        BTreeMap::from([("name".into(), DynamicValue::Text(name.into()))]),
    )
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn atomic_create_receipts() {
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").expect("test PostgreSQL URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap_or_else(|_| panic!("disposable test database connection failed"));
    let namespace = EntityId::new("intenttest").to_string();
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
    // Catch assertion panics in the child so cleanup still happens.
    let result = tokio::spawn(async move { exercise(backend).await }).await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}

async fn exercise(backend: Arc<PgBackend>) {
    let mut schema = definition();
    schema.fields[0]
        .modifiers
        .push(schema_forge_core::types::FieldModifier::Unique);
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    backend
        .prepare_record_revisions(&schema.name)
        .await
        .unwrap();
    let scope = CreateIntentScope {
        principal: "alice".into(),
        tenant: "Organization:one".into(),
        schema: schema.clone(),
    };
    let fingerprint = CreateFingerprint::parse("a".repeat(64)).unwrap();
    let reserve = CreateIntentRequest::Reserve {
        scope: scope.clone(),
        fingerprint: fingerprint.clone(),
    };
    let receipt = backend.create_intent(&reserve).await.unwrap();
    let entity = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("name".into(), DynamicValue::Text("original".into())),
            ("detail".into(), DynamicValue::Text("value".into())),
        ]),
    );
    let commit = CreateIntentRequest::Commit {
        scope: scope.clone(),
        id: receipt.id.clone(),
        fingerprint: fingerprint.clone(),
        entity: entity.clone(),
    };
    let read = CreateIntentRequest::Read {
        scope: scope.clone(),
        id: receipt.id.clone(),
    };
    assert!(backend
        .create_intent(&read)
        .await
        .unwrap()
        .entity_id
        .is_none());
    for changed_scope in [
        CreateIntentScope {
            principal: "bob".into(),
            ..scope.clone()
        },
        CreateIntentScope {
            tenant: "Organization:two".into(),
            ..scope.clone()
        },
    ] {
        assert!(matches!(
            backend
                .create_intent(&CreateIntentRequest::Read {
                    scope: changed_scope.clone(),
                    id: receipt.id.clone()
                })
                .await,
            Err(CreateIntentError::Unavailable)
        ));
        assert!(matches!(
            backend
                .create_intent(&CreateIntentRequest::Commit {
                    scope: changed_scope,
                    id: receipt.id.clone(),
                    fingerprint: fingerprint.clone(),
                    entity: entity.clone()
                })
                .await,
            Err(CreateIntentError::Unavailable)
        ));
    }
    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = vec![];
    for _ in 0..2 {
        let backend = backend.clone();
        let barrier = barrier.clone();
        let commit = commit.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            backend.create_intent(&commit).await.unwrap()
        }));
    }
    let a = tasks.remove(0).await.unwrap();
    let b = tasks.remove(0).await.unwrap();
    assert_eq!(usize::from(a.created) + usize::from(b.created), 1);
    assert_eq!(a.entity_id, b.entity_id);
    let baseline = backend
        .get_versioned(&schema.name, &entity.id)
        .await
        .unwrap();
    assert!(!backend.create_intent(&commit).await.unwrap().created);
    assert_eq!(
        backend
            .get_versioned(&schema.name, &entity.id)
            .await
            .unwrap()
            .revision,
        baseline.revision
    );
    assert!(matches!(
        backend
            .create_intent(&CreateIntentRequest::Commit {
                scope: scope.clone(),
                id: receipt.id.clone(),
                fingerprint: CreateFingerprint::parse("b".repeat(64)).unwrap(),
                entity: entity.clone()
            })
            .await,
        Err(CreateIntentError::ContentConflict)
    ));
    backend.update(&patch(&entity, "renamed")).await.unwrap();
    assert_eq!(
        backend.create_intent(&read).await.unwrap().entity_id,
        Some(entity.id.clone())
    );
    backend.delete(&schema.name, &entity.id).await.unwrap();
    assert_eq!(
        backend.create_intent(&commit).await.unwrap().entity_id,
        Some(entity.id.clone())
    );
    assert!(backend.get(&schema.name, &entity.id).await.is_err());

    // Failed insertion rolls back receipt commitment and its revision together.
    backend.create(&entity).await.unwrap();
    let existing = Entity::new(entity.schema.clone(), entity.fields.clone());
    let pending = backend.create_intent(&reserve).await.unwrap();
    assert!(matches!(
        backend
            .create_intent(&CreateIntentRequest::Commit {
                scope: scope.clone(),
                id: pending.id.clone(),
                fingerprint: fingerprint.clone(),
                entity: existing
            })
            .await,
        Err(CreateIntentError::Backend(
            schema_forge_backend::BackendError::UniqueViolation { .. }
        ))
    ));
    assert!(backend
        .create_intent(&CreateIntentRequest::Read {
            scope: scope.clone(),
            id: pending.id.clone()
        })
        .await
        .unwrap()
        .entity_id
        .is_none());

    sqlx::query("UPDATE _schema_create_intents SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1").bind(pending.id.as_str()).execute(backend.pool()).await.unwrap();
    assert!(matches!(
        backend
            .create_intent(&CreateIntentRequest::Commit {
                scope: scope.clone(),
                id: pending.id,
                fingerprint: fingerprint.clone(),
                entity: entity.clone()
            })
            .await,
        Err(CreateIntentError::Unavailable)
    ));
    assert!(matches!(
        backend
            .create_intent(&CreateIntentRequest::Commit {
                scope: scope.clone(),
                id: CreateIntentId::fresh(),
                fingerprint: fingerprint.clone(),
                entity: entity.clone()
            })
            .await,
        Err(CreateIntentError::Unavailable)
    ));
    sqlx::query("UPDATE _schema_create_intents SET recover_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(receipt.id.as_str()).execute(backend.pool()).await.unwrap();
    assert!(matches!(
        backend.create_intent(&commit).await,
        Err(CreateIntentError::Unavailable)
    ));
    backend.create_intent(&reserve).await.unwrap(); // Prunes the expired receipt.
    assert!(matches!(
        backend.create_intent(&commit).await,
        Err(CreateIntentError::Unavailable)
    ));
    let pending = backend.create_intent(&reserve).await.unwrap();
    let mut changed = scope.clone();
    changed
        .schema
        .annotations
        .push(schema_forge_core::types::Annotation::Version {
            version: schema_forge_core::types::SchemaVersion::new(2).unwrap(),
        });
    backend
        .store_schema_metadata(&changed.schema)
        .await
        .unwrap();
    assert!(matches!(
        backend
            .create_intent(&CreateIntentRequest::Commit {
                scope: changed,
                id: pending.id,
                fingerprint,
                entity
            })
            .await,
        Err(CreateIntentError::SchemaChanged)
    ));
}
