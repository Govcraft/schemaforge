//! External links use the same table-wide constraint on both storage backends.
use std::sync::Arc;

use schema_forge_backend::{
    oauth_identity::ProviderIdentity, user_store::AuthStore, BackendError, EntityAuthStore,
    EntityStore, SchemaBackend,
};
use schema_forge_core::{migration::DiffEngine, system_schemas};

async fn exercise<B: SchemaBackend + EntityStore + 'static>(backend: Arc<B>) {
    let mut definitions = Vec::new();
    for source in [
        system_schemas::USER_SCHEMA,
        system_schemas::OAUTH_IDENTITY_SCHEMA,
    ] {
        let definition = schema_forge_dsl::parse(source).unwrap().remove(0);
        backend
            .apply_migration(&definition.name, &DiffEngine::create_new(&definition).steps)
            .await
            .unwrap();
        backend.store_schema_metadata(&definition).await.unwrap();
        definitions.push(definition);
    }
    backend.finalize_schema_migrations().await.unwrap();
    let user = definitions.remove(0);
    let identity_schema = definitions.remove(0);
    let store = EntityAuthStore::new(backend, user, Arc::new(|_| Some(10)))
        .with_oauth_identity_schema(identity_schema);
    store
        .create_user_without_password("alice@example.com", &["member".into()], "Alice")
        .await
        .unwrap();
    store
        .create_user_without_password("bob@example.com", &["member".into()], "Bob")
        .await
        .unwrap();
    let identity = ProviderIdentity::new("github", "same-subject").unwrap();
    store
        .link_identity("alice@example.com", &identity, "alice@example.com")
        .await
        .unwrap();
    assert!(
        matches!(store.link_identity("bob@example.com", &identity, "bob@example.com").await,
        Err(BackendError::UniqueViolation { schema, field }) if schema == "OAuthIdentity" && field == "identity_key")
    );
    assert_eq!(
        store
            .find_user_by_identity(&identity)
            .await
            .unwrap()
            .unwrap()
            .username,
        "alice@example.com"
    );
    let other_provider = ProviderIdentity::new("google", "same-subject").unwrap();
    store
        .link_identity("bob@example.com", &other_provider, "bob@example.com")
        .await
        .unwrap();
    assert_eq!(
        store.list_identities("alice@example.com").await.unwrap(),
        [identity]
    );
    assert_eq!(
        store.list_identities("bob@example.com").await.unwrap(),
        [other_provider]
    );
    assert!(store
        .validate_credentials("alice@example.com", "anything")
        .await
        .unwrap()
        .is_none());
    store
        .change_password("alice@example.com", "first password")
        .await
        .unwrap();
    assert!(store
        .validate_credentials("alice@example.com", "first password")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn surreal_identity_pair_is_unique() {
    exercise(Arc::new(
        schema_forge_surrealdb::SurrealBackend::connect_memory("oauth_storage", "oauth_storage")
            .await
            .unwrap(),
    ))
    .await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_URL; creates User and OAuthIdentity tables"]
async fn postgres_identity_pair_is_unique() {
    let url =
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").expect("isolated PostgreSQL URL required");
    exercise(Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap(),
    ))
    .await;
}
