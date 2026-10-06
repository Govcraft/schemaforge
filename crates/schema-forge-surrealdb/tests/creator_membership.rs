#[path = "../../schema-forge-backend/tests/support/creator_membership.rs"]
mod contract;

#[tokio::test]
async fn creator_membership_is_atomic() {
    let backend =
        schema_forge_surrealdb::SurrealBackend::connect_memory("onboarding", "onboarding")
            .await
            .unwrap();
    contract::exercise(&backend).await;
}
