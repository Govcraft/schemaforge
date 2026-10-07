//! Unsupported create-intent and authorization contracts on SurrealDB.
use axum::{http::StatusCode, Router};
use std::sync::Arc;

#[path = "support/create_intents.rs"]
pub mod create_intents;
use create_intents::{fixture_with_backend, request};

async fn fixture(owner: &str, roles: &[&str]) -> (Router, String) {
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("conditional", "conditional")
            .await
            .unwrap(),
    );
    fixture_with_backend(backend, owner, roles, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_intents_fail_closed_for_unsupported_and_denied_input() {
    let (app, _) = fixture("editor", &["editor"]).await;
    let (status, _, body) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        serde_json::json!({"title":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "create_intent_unsupported");
    let (status, _, body) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        serde_json::json!({"restricted":"denied"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (app, _) = fixture("editor", &["other"]).await;
    let (status, _, body) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        serde_json::json!({"title":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
