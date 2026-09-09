#[path = "../../schema-forge-backend/tests/support/data_correctness.rs"]
mod contract;

#[tokio::test]
async fn null_filters_stable_pages_and_enum_migrations() {
    let backend =
        schema_forge_surrealdb::SurrealBackend::connect_memory("correctness", "correctness")
            .await
            .unwrap();
    contract::exercise(&backend).await;
}
