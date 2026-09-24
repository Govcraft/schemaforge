#[path = "../../schema-forge-backend/tests/support/migration_renames.rs"]
mod contract;

#[tokio::test]
async fn declared_renames_preserve_values_and_schema_constraints() {
    let backend = schema_forge_surrealdb::SurrealBackend::connect_memory("renames", "renames")
        .await
        .unwrap();
    contract::exercise(&backend).await;
}
