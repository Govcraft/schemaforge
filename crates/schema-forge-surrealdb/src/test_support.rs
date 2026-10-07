//! Isolated databases on the remote server owned by the test runner.

use crate::SurrealBackend;
use schema_forge_backend::BackendError;
use schema_forge_core::types::EntityId;

/// Every call gets its own database, including across nextest processes.
/// Missing setup fails explicitly instead of silently skipping database tests.
pub async fn connect(namespace: &str, prefix: &str) -> Result<SurrealBackend, BackendError> {
    let required = |key| {
        std::env::var(key).map_err(|_| BackendError::ConnectionError {
            message: format!("{key} is required; run the suite through schema-forge-test-runner"),
        })
    };
    let url = required("SCHEMAFORGE_TEST_SURREALDB_URL")?;
    let user = required("SCHEMAFORGE_TEST_SURREALDB_USER")?;
    let password = required("SCHEMAFORGE_TEST_SURREALDB_PASSWORD")?;
    let fixture = EntityId::new("test");
    let namespace = format!("{namespace}_{fixture}");
    let database = format!("{prefix}_{fixture}");
    SurrealBackend::connect_with_auth(&url, &namespace, &database, Some(&user), Some(&password))
        .await
}
