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

#[path = "../../schema-forge-backend/tests/support/creator_membership.rs"]
mod creator_membership;

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn creator_membership_is_atomic() {
    with_database(|backend| async move { creator_membership::exercise(backend.as_ref()).await })
        .await;
}

#[path = "../../schema-forge-backend/tests/support/account_erasure.rs"]
mod account_erasure;

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn account_erasure_rolls_back_restricted_delete_then_removes_dependants() {
    use schema_forge_backend::EntityStore;
    with_database(|backend| async move {
        let fixture = account_erasure::setup(backend.as_ref()).await;
        // Operator-defined references remain protected. The earlier system-row
        // deletes must roll back when this last User delete encounters RESTRICT.
        sqlx::query("CREATE TABLE account_delete_guard (user_id TEXT REFERENCES \"User\"(id) ON DELETE RESTRICT)").execute(backend.pool()).await.unwrap();
        sqlx::query("INSERT INTO account_delete_guard VALUES ($1)").bind(fixture.user.id.as_str()).execute(backend.pool()).await.unwrap();
        assert!(backend.erase_account(&fixture.user.id).await.is_err());
        account_erasure::assert_preserved(backend.as_ref(), &fixture).await;
        sqlx::query("DELETE FROM account_delete_guard").execute(backend.pool()).await.unwrap();
        account_erasure::erase(backend.as_ref(), &fixture).await;
    }).await;
}
