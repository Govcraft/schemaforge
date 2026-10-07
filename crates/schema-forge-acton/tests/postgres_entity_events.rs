//! Committed CRUD and reconciled create-intent event projection on PostgreSQL.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use schema_forge_backend::SchemaBackend;
use std::sync::Arc;
use tower::ServiceExt;
#[path = "support/entity_events.rs"]
pub mod entity_events;
#[path = "support/postgres.rs"]
mod postgres;
use entity_events::{
    change, connect, enabled, exercise_crud, fixture_backend, frame, json, request,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_URL; creates Note and User tables"]
async fn postgres_crud_events_equal_authorized_get() {
    let url = postgres::isolated_url("SCHEMAFORGE_TEST_POSTGRES_EVENTS_URL");
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("could not connect to the isolated PostgreSQL namespace")),
    );
    let f = fixture_backend(backend.clone(), enabled()).await;
    backend
        .prepare_record_revisions(&schema_forge_core::types::SchemaName::new("Note").unwrap())
        .await
        .unwrap();
    exercise_crud(&f).await;
    let mut body = connect(&f.app, "").await;
    let fields = serde_json::json!({"title":"Intent create", "category":"books"});
    let receipt = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/create-intents",
            fields.clone(),
        )
        .await,
    )
    .await;
    let id = receipt["id"].as_str().unwrap();
    for attempt in 0..2 {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/schemas/Note/entities")
                    .header("content-type", "application/json")
                    .header("create-intent", id)
                    .body(Body::from(serde_json::json!({"fields":fields}).to_string()))
                    .unwrap_or_else(|_| {
                        panic!("could not connect to the isolated PostgreSQL namespace")
                    }),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt == 0 {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            }
        );
        let detail = json(response).await;
        if attempt == 0 {
            let (_, event) = change(&mut body).await;
            assert_eq!(event["entity"], detail);
            assert_eq!(event["actor"], "user:alice");
        }
    }
    assert!(
        frame(&mut body).await.contains("keep-alive"),
        "reconciliation must not publish a duplicate create"
    );
}
