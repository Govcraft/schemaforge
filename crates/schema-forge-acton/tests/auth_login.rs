//! Integration tests for the JSON login endpoint (`POST /auth/login`).
//!
//! These tests mount **only** the auth sub-router (`auth_routes()`) onto a
//! synthetic axum router with the two Extensions the handler depends on:
//! a real in-memory SurrealBackend (playing the `DynAuthStore` role) and a
//! freshly minted PasetoGenerator backed by a 32-byte symmetric key.
//!
//! Scope cut: end-to-end verification that the minted token actually passes
//! the acton-service token middleware on a downstream protected route is
//! covered by the shell-side smoke test (see the task report) because
//! mounting the real middleware here requires the full ServiceBuilder
//! pipeline, which is the wrong level of fidelity for this file.

#![cfg(feature = "surrealdb")]

use std::sync::Arc;

use acton_service::auth::config::{PasetoGenerationConfig, TokenGenerationConfig};
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use schema_forge_acton::routes::auth_routes;
use schema_forge_acton::state::DynAuthStore;
use schema_forge_backend::AuthStore;
use schema_forge_surrealdb::SurrealBackend;
use tempfile::NamedTempFile;
use tower::ServiceExt;

/// Seed an in-memory SurrealBackend with one known-good admin user.
async fn seeded_auth_store() -> Arc<dyn DynAuthStore> {
    let backend = SurrealBackend::connect_memory("test", "auth_login_test")
        .await
        .expect("connect in-memory surreal");
    AuthStore::create_user(&backend, "admin", "dev", &["admin".to_string()], "Administrator")
        .await
        .expect("seed admin user");
    Arc::new(backend)
}

/// Write a random 32-byte V4.local key to a NamedTempFile and build a
/// matching `PasetoGenerator`. The tempfile is returned so the caller keeps
/// it alive for the duration of the test.
fn build_test_generator() -> (Arc<PasetoGenerator>, NamedTempFile) {
    let tmp = NamedTempFile::new().expect("tempfile");
    let key_bytes: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ 0xA5);
    std::fs::write(tmp.path(), key_bytes).expect("write key");

    let paseto_gen = PasetoGenerationConfig {
        version: "v4".to_string(),
        purpose: "local".to_string(),
        key_path: tmp.path().to_path_buf(),
        issuer: Some("schemaforge-test".to_string()),
        audience: None,
    };
    let token_gen = TokenGenerationConfig {
        access_token_lifetime_secs: 3600,
        issuer: Some("schemaforge-test".to_string()),
        audience: None,
        include_jti: true,
    };
    let generator = PasetoGenerator::new(&paseto_gen, &token_gen).expect("build generator");
    (Arc::new(generator), tmp)
}

/// Build a router that mounts only `/auth/login` with the two Extensions.
async fn login_app() -> (Router, NamedTempFile) {
    let auth_store = seeded_auth_store().await;
    let (generator, key_tmp) = build_test_generator();
    let router = auth_routes()
        .layer(Extension(auth_store))
        .layer(Extension(generator))
        .with_state(acton_service::state::AppState::<
            schema_forge_acton::SchemaForgeConfig,
        >::default());
    (router, key_tmp)
}

async fn post_login(app: Router, body: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).expect("response body is JSON");
    (status, body)
}

#[tokio::test]
async fn login_with_correct_credentials_returns_token() {
    let (app, _key) = login_app().await;
    let (status, body) =
        post_login(app, r#"{"username":"admin","password":"dev"}"#).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().expect("token field is a string");
    assert!(
        token.starts_with("v4.local."),
        "expected v4.local.* token, got {token}"
    );
    let expires_at = body["expires_at"]
        .as_str()
        .expect("expires_at field is a string");
    let parsed = chrono::DateTime::parse_from_rfc3339(expires_at)
        .expect("expires_at parses as RFC3339");
    assert!(
        parsed > chrono::Utc::now(),
        "expires_at {expires_at} should be in the future"
    );
}

#[tokio::test]
async fn login_with_wrong_password_returns_401_envelope() {
    let (app, _key) = login_app().await;
    let (status, body) =
        post_login(app, r#"{"username":"admin","password":"wrong"}"#).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid credentials");
    assert_eq!(body["code"], "UNAUTHORIZED");
    assert_eq!(body["status"], 401);
}

#[tokio::test]
async fn login_with_unknown_user_returns_401_envelope() {
    let (app, _key) = login_app().await;
    let (status, body) =
        post_login(app, r#"{"username":"ghost","password":"dev"}"#).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid credentials");
    assert_eq!(body["code"], "UNAUTHORIZED");
    assert_eq!(body["status"], 401);
}
