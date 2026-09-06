//! Opt-in live PostgreSQL concurrency checks, isolated in a disposable namespace.

use std::{collections::BTreeMap, sync::Arc};

use schema_forge_backend::{
    conditional::ConditionalMutationError, Entity, EntityStore, SchemaBackend,
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
async fn atomic_record_mutations_and_readiness() {
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").expect("test PostgreSQL URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect test database");
    let namespace = EntityId::new("cas").to_string();
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
    let schema = definition();
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let entity = backend
        .create(&Entity::new(
            schema.name.clone(),
            BTreeMap::from([
                ("name".into(), DynamicValue::Text("original".into())),
                ("detail".into(), DynamicValue::Text("preserved".into())),
            ]),
        ))
        .await
        .unwrap();
    assert!(matches!(
        backend.get_versioned(&schema.name, &entity.id).await,
        Err(ConditionalMutationError::Unsupported)
    ));
    backend
        .prepare_record_revisions(&schema.name)
        .await
        .unwrap();
    let initial = backend
        .get_versioned(&schema.name, &entity.id)
        .await
        .unwrap();
    backend
        .prepare_record_revisions(&schema.name)
        .await
        .unwrap();
    assert_eq!(
        backend
            .get_versioned(&schema.name, &entity.id)
            .await
            .unwrap()
            .revision,
        initial.revision
    );

    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = vec![];
    for name in ["first", "second"] {
        let backend = backend.clone();
        let barrier = barrier.clone();
        let change = patch(&entity, name);
        let expected = initial.revision.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            backend.update_if(&change, &expected).await
        }));
    }
    let first = tasks.remove(0).await.unwrap();
    let second = tasks.remove(0).await.unwrap();
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(
        matches!(first, Err(ConditionalMutationError::Conflict))
            || matches!(second, Err(ConditionalMutationError::Conflict))
    );
    let winner = first.or(second).unwrap();
    assert_eq!(winner.entity.field("detail"), entity.field("detail"));
    assert_ne!(winner.revision, initial.revision);
    assert_eq!(
        winner.entity.fields.len(),
        2,
        "internal markers never become user fields"
    );

    backend
        .update(&patch(&entity, "unconditional"))
        .await
        .unwrap();
    assert!(matches!(
        backend
            .update_if(&patch(&entity, "stale"), &winner.revision)
            .await,
        Err(ConditionalMutationError::Conflict)
    ));
    let baseline = backend
        .get_versioned(&schema.name, &entity.id)
        .await
        .unwrap();
    let noop = Entity::with_id(entity.id.clone(), schema.name.clone(), BTreeMap::new());
    let accepted = backend.update_if(&noop, &baseline.revision).await.unwrap();
    assert_ne!(accepted.revision, baseline.revision);
    assert!(matches!(
        backend.update_if(&noop, &baseline.revision).await,
        Err(ConditionalMutationError::Conflict)
    ));

    let barrier = Arc::new(Barrier::new(2));
    let deleting = backend.clone();
    let delete_entity = entity.clone();
    let delete_revision = accepted.revision.clone();
    let waiting = barrier.clone();
    let deletion = tokio::spawn(async move {
        waiting.wait().await;
        deleting
            .delete_if(&delete_entity.schema, &delete_entity.id, &delete_revision)
            .await
    });
    barrier.wait().await;
    let update = backend
        .update_if(&patch(&entity, "racing delete"), &accepted.revision)
        .await;
    let deletion = deletion.await.unwrap();
    assert_eq!(
        usize::from(update.is_ok()) + usize::from(deletion.is_ok()),
        1
    );
    if let Ok(updated) = update {
        backend
            .delete_if(&schema.name, &entity.id, &updated.revision)
            .await
            .unwrap();
    }
    backend.create(&entity).await.unwrap();
    assert!(matches!(
        backend
            .delete_if(&schema.name, &entity.id, &accepted.revision)
            .await,
        Err(ConditionalMutationError::Conflict)
    ));

    // A transaction containing data/schema migration invalidates row baselines.
    let before_migration = backend
        .get_versioned(&schema.name, &entity.id)
        .await
        .unwrap();
    let mut evolved = schema.clone();
    evolved.fields.push(FieldDefinition::new(
        FieldName::new("extra").unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    ));
    backend
        .apply_migration(&schema.name, &DiffEngine::diff(&schema, &evolved).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&evolved).await.unwrap();
    assert!(matches!(
        backend
            .update_if(
                &patch(&entity, "stale migration"),
                &before_migration.revision
            )
            .await,
        Err(ConditionalMutationError::Conflict)
    ));
    let current = backend
        .get_versioned(&schema.name, &entity.id)
        .await
        .unwrap();
    let a = backend.delete_if(&schema.name, &entity.id, &current.revision);
    let b = backend.delete_if(&schema.name, &entity.id, &current.revision);
    let (a, b) = tokio::join!(a, b);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(
        matches!(a, Err(ConditionalMutationError::Conflict))
            || matches!(b, Err(ConditionalMutationError::Conflict))
    );
}
