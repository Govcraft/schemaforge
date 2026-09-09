use schema_forge_core::types::EntityId;
use schema_forge_postgres::PgBackend;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
#[path = "../../schema-forge-backend/tests/support/data_correctness.rs"]
mod contract;
#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn null_filters_stable_pages_and_enum_migrations() {
    with_database(|backend| async move { contract::exercise(backend.as_ref()).await }).await;
}
async fn with_database<Test, Pending>(test: Test)
where
    Test: FnOnce(Arc<PgBackend>) -> Pending,
    Pending: std::future::Future<Output = ()> + Send + 'static,
{
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").expect("test PostgreSQL URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect test database");
    let namespace = EntityId::new("cas").to_string();
    sqlx::query(&format!("CREATE SCHEMA \"{namespace}\""))
        .execute(&admin)
        .await
        .unwrap();
    let scope = namespace.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let scope = scope.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path', $1, false)")
                    .bind(scope)
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let backend = Arc::new(PgBackend::from_pool(pool.clone()).await.unwrap());
    // Catch assertion panics in the child so cleanup still happens.
    let result = tokio::spawn(test(backend)).await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
