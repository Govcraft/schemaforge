//! Integration tests for `/api/v1/forge/users` endpoints.
//!
//! These exercise the full router wiring (state + Extension + Claims
//! injection) against an in-memory `SurrealBackend` seeded with an admin
//! user and, where relevant, a non-admin "alice". The harness follows the
//! same pattern as `tests/integration.rs` but additionally layers the
//! `auth_store` Extension the way `commands::serve::build_versioned_routes`
//! does in production.
//!
//! Cases (matching the task spec):
//! 1. Admin creates a non-admin user via `POST /users`, then `GET /users`
//!    returns both users.
//! 2. Non-admin gets 403 from `POST /users`.
//! 3. Admin deletes a user via `DELETE /users/:username`, then `GET /users`
//!    no longer returns them.
//! 4. Self-password change works without the admin role.

#![cfg(feature = "surrealdb")]

use std::collections::HashMap;
use std::sync::Arc;

use acton_service::middleware::Claims;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::DynAuthStore;
use schema_forge_backend::AuthStore;
use schema_forge_surrealdb::SurrealBackend;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn make_claims(sub: &str, roles: &[&str]) -> Claims {
    Claims {
        sub: sub.to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    }
}

async fn seed_backend(namespace: &str) -> Arc<SurrealBackend> {
    let backend = SurrealBackend::connect_memory("test", namespace)
        .await
        .expect("connect in-memory surreal");
    AuthStore::create_user(
        &backend,
        "admin",
        "adminpass",
        &["admin".to_string()],
        "Administrator",
    )
    .await
    .expect("seed admin user");
    Arc::new(backend)
}

fn users_router(backend: Arc<SurrealBackend>, claims: Claims) -> Router {
    let auth_store: Arc<dyn DynAuthStore> = backend;
    forge_routes()
        .layer(Extension(auth_store))
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let claims = claims.clone();
                async move {
                    req.extensions_mut().insert(claims);
                    next.run(req).await
                }
            },
        ))
        .with_state(acton_service::state::AppState::<SchemaForgeConfig>::default())
}

async fn json_request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let body_bytes = match body {
        Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
        None => Body::empty(),
    };
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body_bytes)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_can_create_and_list_users() {
    let backend = seed_backend("users_create_list").await;
    let app = users_router(backend, make_claims("user:admin", &["admin"]));

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/users",
        Some(serde_json::json!({
            "username": "alice",
            "password": "alicepass",
            "roles": ["sales"],
            "display_name": "Alice"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {json}");
    assert_eq!(json["username"], "alice");
    assert_eq!(json["roles"], serde_json::json!(["sales"]));
    assert_eq!(json["active"], true);

    let (status, json) = json_request(&app, Method::GET, "/users", None).await;
    assert_eq!(status, StatusCode::OK, "body: {json}");
    assert_eq!(json["count"], 2);
    let names: Vec<&str> = json["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"admin"));
    assert!(names.contains(&"alice"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_admin_cannot_create_user() {
    let backend = seed_backend("users_forbidden").await;
    AuthStore::create_user(
        backend.as_ref(),
        "alice",
        "alicepass",
        &["sales".to_string()],
        "Alice",
    )
    .await
    .unwrap();

    let app = users_router(backend, make_claims("user:alice", &["sales"]));
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/users",
        Some(serde_json::json!({
            "username": "mallory",
            "password": "mallorypass",
            "roles": []
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_can_delete_user() {
    let backend = seed_backend("users_delete").await;
    AuthStore::create_user(
        backend.as_ref(),
        "alice",
        "alicepass",
        &["sales".to_string()],
        "Alice",
    )
    .await
    .unwrap();

    let app = users_router(backend, make_claims("user:admin", &["admin"]));
    let (status, _) = json_request(&app, Method::DELETE, "/users/alice", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, json) = json_request(&app, Method::GET, "/users", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = json["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"alice"));
    assert!(names.contains(&"admin"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_can_change_own_password_without_admin() {
    let backend = seed_backend("users_self_password").await;
    AuthStore::create_user(
        backend.as_ref(),
        "alice",
        "oldpassword",
        &["sales".to_string()],
        "Alice",
    )
    .await
    .unwrap();

    let app = users_router(backend.clone(), make_claims("user:alice", &["sales"]));
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/users/alice/password",
        Some(serde_json::json!({ "password": "newpassword" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {json}");

    let ok = AuthStore::validate_credentials(backend.as_ref(), "alice", "newpassword")
        .await
        .unwrap();
    assert!(ok.is_some(), "expected new password to validate");
    let stale = AuthStore::validate_credentials(backend.as_ref(), "alice", "oldpassword")
        .await
        .unwrap();
    assert!(stale.is_none(), "expected old password to be invalidated");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_user_password_change_is_forbidden_for_non_admin() {
    let backend = seed_backend("users_cross_password").await;
    AuthStore::create_user(
        backend.as_ref(),
        "alice",
        "alicepass",
        &["sales".to_string()],
        "Alice",
    )
    .await
    .unwrap();
    AuthStore::create_user(
        backend.as_ref(),
        "bob",
        "bobpass12",
        &["sales".to_string()],
        "Bob",
    )
    .await
    .unwrap();

    let app = users_router(backend, make_claims("user:alice", &["sales"]));
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/users/bob/password",
        Some(serde_json::json!({ "password": "newbobpass" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
}
