//! Regression coverage for side-effect-free planning and literal text matching.
use schema_forge_backend::traits::SchemaBackend;
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{DynamicValue, EntityId, SchemaId, SchemaName};
use schema_forge_postgres::{query::query_to_sql, PgBackend};
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
async fn connection_errors_do_not_expose_credentials_or_driver_input() {
    for url in [
        "postgres://private-user:private-password@localhost:invalid/db",
        "postgres://private-user:private-password@localhost/db?port=private-password",
    ] {
        for result in [
            PgBackend::connect(url).await,
            PgBackend::connect_read_only(url).await,
            PgBackend::connect_with_max_connections(url, 1).await,
        ] {
            let error = result
                .err()
                .expect("malformed connection must fail")
                .to_string();
            assert!(!error.contains("private-password"), "{error}");
            assert!(!error.contains("private-user"), "{error}");
            assert!(!error.contains(url), "{error}");
        }
    }
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA and CREATEROLE privileges"]
async fn planning_connections_do_not_create_tables_and_read_only_roles_can_inspect() {
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let namespace = EntityId::new("planning").to_string();
    let role = EntityId::new("reader").to_string();
    sqlx::query(&format!("CREATE SCHEMA \"{namespace}\""))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE ROLE \"{role}\""))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA \"{namespace}\" TO \"{role}\""
    ))
    .execute(&admin)
    .await
    .unwrap();
    let scope = namespace.clone();
    let reader = role.clone();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let scope = scope.clone();
            let reader = reader.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET ROLE \"{reader}\""))
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SELECT set_config('search_path', $1, false)")
                    .bind(scope)
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let backend = PgBackend::from_pool_read_only(pool.clone());
    let name = SchemaName::new("Example").unwrap();
    let connector_url = format!(
        "{url}{}options=-csearch_path%3D{namespace}",
        if url.contains('?') { "&" } else { "?" }
    );
    let planning = PgBackend::connect_read_only(&connector_url).await.unwrap();
    assert!(planning.list_schema_metadata().await.unwrap().is_empty());
    planning.pool().close().await;
    let fixture_admin = admin.clone();
    let fixture_namespace = namespace.clone();
    let fixture_role = role.clone();
    let result = tokio::spawn(async move {
        assert!(backend.list_schema_metadata().await.unwrap().is_empty());
        assert!(backend.load_schema_metadata(&name).await.unwrap().is_none());
        // The write constructor demonstrates this role really cannot bootstrap.
        assert!(PgBackend::from_pool(backend.pool().clone()).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = $1")
            .bind(&fixture_namespace).fetch_one(&fixture_admin).await.unwrap();
        assert_eq!(count, 0, "planning must not create bookkeeping tables");
        sqlx::query(&format!("CREATE TABLE \"{fixture_namespace}\"._schema_metadata (name TEXT PRIMARY KEY, definition JSONB NOT NULL)"))
            .execute(&fixture_admin).await.unwrap();
        sqlx::query(&format!("GRANT SELECT ON \"{fixture_namespace}\"._schema_metadata TO \"{fixture_role}\""))
            .execute(&fixture_admin).await.unwrap();
        let existing = PgBackend::from_pool_read_only(backend.pool().clone());
        assert!(existing.list_schema_metadata().await.unwrap().is_empty());
        assert!(existing.load_schema_metadata(&name).await.unwrap().is_none());
    })
    .await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = $1")
        .bind(&namespace)
        .fetch_one(&admin)
        .await
        .unwrap();
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("DROP ROLE \"{role}\""))
        .execute(&admin)
        .await
        .unwrap();
    result.unwrap();
    assert_eq!(count, 1, "only the fixture metadata table should exist");
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL"]
async fn postgres_text_filters_are_literal_and_case_sensitive() {
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("CREATE TEMP TABLE filter_literals (id TEXT, value TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    for (id, value) in [
        ("one", "A%_\\tail"),
        ("two", "aXXtail"),
        ("three", "Azzztail"),
    ] {
        sqlx::query("INSERT INTO filter_literals VALUES ($1, $2)")
            .bind(id)
            .bind(value)
            .execute(&pool)
            .await
            .unwrap();
    }
    for (filter, expected) in [
        (
            Filter::contains(FieldPath::single("value"), "%_\\"),
            vec!["one"],
        ),
        (
            Filter::starts_with(FieldPath::single("value"), "A%_\\"),
            vec!["one"],
        ),
        (Filter::contains(FieldPath::single("value"), "axx"), vec![]),
    ] {
        let compiled = query_to_sql(
            &Query::new(SchemaId::new()).with_filter(filter),
            "filter_literals",
        );
        let DynamicValue::Text(pattern) = &compiled.params[0] else {
            panic!("text pattern expected")
        };
        let rows: Vec<(String, String)> = sqlx::query_as(&compiled.sql)
            .bind(pattern)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(
            rows.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            expected
        );
    }
    pool.close().await;
}
