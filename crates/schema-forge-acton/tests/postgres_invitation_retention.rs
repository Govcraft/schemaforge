//! Invitation retention on an isolated PostgreSQL database.
use std::sync::Arc;
#[path = "support/invitation_retention.rs"]
mod invitation_retention;
#[path = "support/postgres.rs"]
mod postgres;

#[tokio::test]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_RETENTION_URL; creates ForgeInvitation"]
async fn postgres_invitation_retention_and_concurrent_acceptance() {
    let url = postgres::isolated_url("SCHEMAFORGE_TEST_POSTGRES_RETENTION_URL");
    invitation_retention::exercise(Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap(),
    ))
    .await;
}
