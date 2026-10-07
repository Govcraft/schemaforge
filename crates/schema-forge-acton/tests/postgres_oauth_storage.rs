//! External-identity uniqueness and password transition contract on PostgreSQL.
use std::sync::Arc;
#[path = "support/oauth_storage.rs"]
pub mod oauth_storage;
#[path = "support/postgres.rs"]
mod postgres;
use oauth_storage::exercise;

#[tokio::test]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_URL; creates User and OAuthIdentity tables"]
async fn postgres_identity_pair_is_unique() {
    let url = postgres::isolated_url("SCHEMAFORGE_TEST_POSTGRES_IDENTITY_URL");
    exercise(Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("could not connect to the isolated PostgreSQL namespace")),
    ))
    .await;
}
