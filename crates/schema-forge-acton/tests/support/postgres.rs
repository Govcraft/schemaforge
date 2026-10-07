//! Resolve an explicitly isolated test database URL without logging credentials.
pub fn isolated_url(target_variable: &str) -> String {
    std::env::var(target_variable)
        .or_else(|_| std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL"))
        .unwrap_or_else(|_| {
            panic!("{target_variable} or SCHEMAFORGE_TEST_POSTGRES_URL must name an isolated PostgreSQL namespace")
        })
}
