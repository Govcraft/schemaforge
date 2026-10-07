//! Invitation retention on a disposable remote SurrealDB server.
use std::sync::Arc;
#[path = "support/invitation_retention.rs"]
mod invitation_retention;

#[tokio::test]
async fn surreal_invitation_retention_and_concurrent_acceptance() {
    invitation_retention::exercise(Arc::new(
        schema_forge_surrealdb::test_support::connect(
            "invitation_retention",
            "invitation_retention",
        )
        .await
        .unwrap(),
    ))
    .await;
}
