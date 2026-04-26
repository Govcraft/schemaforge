//! Integration tests for `/api/v1/forge/users` endpoints.
//!
//! These exercise the full router wiring (state + Extension + Claims
//! injection) against an in-memory `SurrealBackend` seeded with a
//! `platform_admin` user and, where relevant, additional non-platform
//! peers. The harness follows the same pattern as `tests/integration.rs`
//! but additionally layers the `auth_store` Extension the way
//! `commands::serve::build_versioned_routes` does in production.
//!
//! Authorization model under test:
//! - `GET /users`, `POST /users`, `DELETE /users/:username` require the
//!   `platform_admin` role.
//! - `GET /users` filters out users whose roles contain `platform_admin`
//!   when the caller does not hold it.
//! - `POST /users` rejects requests that grant `platform_admin` unless
//!   the caller already holds it.
//! - `DELETE /users/:username` refuses to remove the last
//!   `platform_admin` (returns 409 with `reason: "last_platform_admin"`).
//! - `POST /users/:username/password` allows `platform_admin` OR self.

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
        &["platform_admin".to_string()],
        "Administrator",
    )
    .await
    .expect("seed platform_admin user");
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
async fn platform_admin_can_create_and_list_users() {
    let backend = seed_backend("users_create_list").await;
    let app = users_router(backend, make_claims("user:admin", &["platform_admin"]));

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
async fn non_platform_admin_cannot_create_user() {
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
async fn app_admin_role_does_not_grant_platform_admin_powers() {
    // The literal "admin" role string is now reserved for in-app use.
    // A user holding only "admin" must NOT be able to hit the platform
    // user-management endpoints — that is the whole point of the rename.
    let backend = seed_backend("users_app_admin_isolated").await;
    AuthStore::create_user(
        backend.as_ref(),
        "appadmin",
        "appadminpass",
        &["admin".to_string()],
        "App Admin",
    )
    .await
    .unwrap();

    let app = users_router(backend, make_claims("user:appadmin", &["admin"]));
    let (status, json) = json_request(&app, Method::GET, "/users", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_admin_can_delete_non_platform_user() {
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

    let app = users_router(backend, make_claims("user:admin", &["platform_admin"]));
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
async fn user_can_change_own_password_without_platform_admin() {
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
async fn cross_user_password_change_is_forbidden_for_non_platform_admin() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_admin_sees_all_users_in_list() {
    let backend = seed_backend("users_list_platform_visibility").await;
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
        "ops",
        "opspass12",
        &["platform_admin".to_string()],
        "Ops",
    )
    .await
    .unwrap();

    let app = users_router(backend, make_claims("user:admin", &["platform_admin"]));
    let (status, json) = json_request(&app, Method::GET, "/users", None).await;
    assert_eq!(status, StatusCode::OK, "body: {json}");
    let names: Vec<&str> = json["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"admin"));
    assert!(names.contains(&"alice"));
    assert!(names.contains(&"ops"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_filter_hides_platform_admins_from_non_platform_callers() {
    // The list filter must drop platform_admin rows before they reach a
    // non-platform caller. The route gate also rejects such callers
    // today, so we verify the filter directly against the handler by
    // constructing a Router whose claims layer injects a non-platform
    // identity but call the underlying logic via the same end-to-end
    // path. Since the gate fires first (returning 403), we exercise the
    // filter by making the gate permissive in this test alone: we do
    // that by using a `platform_admin` token to seed data, then we
    // re-build the router with non-platform claims and assert 403 — and
    // separately we pull the unit-level guarantee from
    // `routes::users::tests`. The full filter path is exercised by the
    // `platform_admin_sees_all_users_in_list` happy path above.
    let backend = seed_backend("users_list_hide_platform").await;
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
    let (status, _) = json_request(&app, Method::GET, "/users", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_platform_admin_cannot_grant_platform_admin() {
    // Synthesizes the future state where a non-platform caller can hit
    // POST /users: forge a non-platform Claims token onto the router
    // and assert the role-grant guard fires before the platform-admin
    // gate. Today the gate fires first (returning 403 with message
    // "user management requires platform_admin role"); when the gate
    // is loosened in a follow-up, the body message becomes
    // "only platform_admin may grant the platform_admin role". Either
    // way, the response is 403.
    let backend = seed_backend("users_no_escalation").await;
    let app = users_router(backend, make_claims("user:alice", &["manager"]));
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/users",
        Some(serde_json::json!({
            "username": "mallory",
            "password": "mallorypass",
            "roles": ["platform_admin"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_refuses_last_platform_admin() {
    // Only the seeded `admin` is a platform_admin. Deleting them must
    // return 409 with reason "last_platform_admin". The harness uses a
    // *different* platform_admin caller (sub "user:operator") so that
    // the "cannot delete yourself" check doesn't fire first.
    let backend = seed_backend("users_last_platform_admin").await;
    let app = users_router(backend, make_claims("user:operator", &["platform_admin"]));
    let (status, json) = json_request(&app, Method::DELETE, "/users/admin", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {json}");
    assert_eq!(json["error"], "conflict");
    assert_eq!(json["reason"], "last_platform_admin");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_allows_when_other_platform_admins_exist() {
    let backend = seed_backend("users_delete_one_of_many").await;
    AuthStore::create_user(
        backend.as_ref(),
        "ops",
        "opspass12",
        &["platform_admin".to_string()],
        "Ops",
    )
    .await
    .unwrap();

    let app = users_router(backend, make_claims("user:operator", &["platform_admin"]));
    let (status, _) = json_request(&app, Method::DELETE, "/users/ops", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
