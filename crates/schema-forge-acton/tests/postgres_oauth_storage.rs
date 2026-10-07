//! External-identity uniqueness and password transition contract on PostgreSQL.
use std::sync::Arc;
#[path = "support/oauth_storage.rs"]
pub mod oauth_storage;
#[path = "support/postgres.rs"]
mod postgres;
use oauth_storage::exercise;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_URL; creates User and OAuthIdentity tables"]
async fn postgres_identity_pair_is_unique_and_account_erasure_removes_dependants() {
    let url = postgres::isolated_url("SCHEMAFORGE_TEST_POSTGRES_IDENTITY_URL");
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("could not connect to the isolated PostgreSQL namespace")),
    );
    exercise(backend.clone()).await;
    erase_populated_account_through_http(backend).await;
}

#[path = "support/conditional_entities.rs"]
pub mod conditional_entities;

async fn erase_populated_account_through_http(backend: Arc<schema_forge_postgres::PgBackend>) {
    use axum::{http::StatusCode, Extension};
    use chrono::{Duration, Utc};
    use schema_forge_acton::state::DynForgeBackend;
    use schema_forge_backend::{AuthStore, EntityAuthStore, EntityStore, NewInvitation};
    use schema_forge_core::{
        query::{FieldPath, Filter, Query},
        types::{DynamicValue, SchemaName},
    };
    let mut registry = std::collections::HashMap::new();
    schema_forge_acton::system::seed_system_schemas_into_map(&mut registry, backend.as_ref())
        .await
        .unwrap();
    let user_schema = registry["User"].clone();
    let identity_schema = registry["OAuthIdentity"].clone();
    let membership_schema = registry["TenantMembership"].clone();
    let store = Arc::new(
        EntityAuthStore::new(backend.clone(), user_schema.clone(), Arc::new(|_| Some(10)))
            .with_oauth_identity_schema(identity_schema.clone())
            .with_tenant_membership_schema(membership_schema.clone()),
    );
    store
        .add_tenant_membership(
            "alice@example.com",
            "Organization",
            "tenant",
            Some("member"),
        )
        .await
        .unwrap();
    let user = store
        .get_user_entity("alice@example.com")
        .await
        .unwrap()
        .unwrap();
    let invites =
        schema_forge_acton::system::provision_invite_store(backend.as_ref(), backend.clone())
            .await
            .unwrap();
    let invitation = invites
        .create(NewInvitation {
            email: "alice@example.com".into(),
            display_name: None,
            tenant_type: None,
            tenant_id: None,
            role: None,
            jti: "account-erasure".into(),
            token: "PRIVATE".into(),
            expires_at: Utc::now() + Duration::days(1),
            invited_by: Some("operator".into()),
        })
        .await
        .unwrap();
    invites
        .mark_consumed(&invitation.id, Utc::now())
        .await
        .unwrap();
    let entities = |schema: &schema_forge_core::types::SchemaDefinition| {
        Query::new(schema.id.clone()).with_filter(Filter::eq(
            FieldPath::single("user"),
            DynamicValue::Ref(user.id.clone()),
        ))
    };
    assert_eq!(
        backend
            .query(&entities(&identity_schema))
            .await
            .unwrap()
            .entities
            .len(),
        1
    );
    assert_eq!(
        backend
            .query(&entities(&membership_schema))
            .await
            .unwrap()
            .entities
            .len(),
        1
    );
    let dynamic: Arc<dyn DynForgeBackend> = backend.clone();
    let auth: Arc<dyn schema_forge_acton::state::DynAuthStore> = store.clone();
    let app = conditional_entities::app_with_backend(dynamic, user_schema, &["platform_admin"])
        .await
        .layer(Extension(auth));
    let (status, _, body) = conditional_entities::request(
        &app,
        "/users/alice@example.com",
        "DELETE",
        None,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(backend
        .get(&SchemaName::new("User").unwrap(), &user.id)
        .await
        .is_err());
    assert!(backend
        .query(&entities(&identity_schema))
        .await
        .unwrap()
        .entities
        .is_empty());
    assert!(backend
        .query(&entities(&membership_schema))
        .await
        .unwrap()
        .entities
        .is_empty());
    assert!(invites
        .find_by_jti("account-erasure")
        .await
        .unwrap()
        .is_none());
    assert!(store.get_user("bob@example.com").await.unwrap().is_some());
}
