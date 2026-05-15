//! Integration tests for `/health` version reporting (issue #55).
//!
//! `acton-service`'s default `/health` handler reports the `acton-service`
//! crate's `CARGO_PKG_VERSION` (currently `"0.23.0"`), not the running
//! `schema-forge-acton` version. SchemaForge installs an axum middleware
//! (`schema_forge_health_middleware`) that intercepts `GET /health` and
//! replies with the correct version so `/health` agrees with
//! `/api/v1/forge/meta` and `schema-forge --version`.
//!
//! These tests exercise the exact router wiring `schema-forge-cli`'s `serve`
//! command builds in production: a `VersionedApiBuilder`-produced router
//! with `meta_routes()` mounted under `/api/v1/forge/`, wrapped by the
//! SchemaForge health middleware, then state-attached via `ServiceBuilder`.

#![cfg(feature = "surrealdb")]

use std::sync::Arc;

use acton_service::config::Config;
use acton_service::service_builder::VersionedRoutes;
use acton_service::versioning::{ApiVersion, VersionedApiBuilder};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_acton::routes::meta_routes;
use schema_forge_acton::{
    schema_forge_health_middleware, MetaInfo, SCHEMA_FORGE_ACTON_VERSION,
    SCHEMA_FORGE_SERVICE_NAME,
};
use tower::ServiceExt;

/// Build a router that mirrors `serve.rs`'s wiring without going through
/// `ServiceBuilder::build()` (whose constructed `Router` is private):
///   1. `VersionedApiBuilder` produces a router with acton-service's
///      default `/health` + `/ready` plus the `/api/v1/forge/...` tree.
///   2. The SchemaForge `MetaInfo` is layered as an `Extension` so
///      `GET /api/v1/forge/meta` has data to return.
///   3. The SchemaForge health middleware wraps the whole router,
///      intercepting `GET /health` before acton-service's handler sees it.
///   4. Typed `AppState` (carrying the SchemaForge service name) is
///      attached via `Router::with_state`.
async fn build_router() -> Router {
    let meta = Arc::new(MetaInfo::new("surrealdb", "SurrealDB (test)", 3600));

    let versioned = VersionedApiBuilder::<SchemaForgeConfig>::with_config()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, move |_router| {
            // Mount only `/meta` under `/api/v1/forge/` — that's the
            // surface this test cares about. The other forge routes
            // require a full ForgeActor, which `tests/users.rs` already
            // covers and is unrelated to issue #55.
            let forge: Router<acton_service::state::AppState<SchemaForgeConfig>> =
                Router::new().nest("/forge", meta_routes());
            forge.layer(Extension(meta.clone()))
        })
        .build_routes();

    let stateful = match versioned {
        VersionedRoutes::WithState(router) => router,
        VersionedRoutes::WithoutState(_) => {
            panic!("VersionedApiBuilder always returns WithState in this configuration")
        }
    };

    // Apply the SchemaForge health middleware so `GET /health` reports
    // the schema-forge-acton crate version (issue #55).
    let stateful = stateful.layer(axum::middleware::from_fn(
        schema_forge_health_middleware,
    ));

    // The middleware reports `SCHEMA_FORGE_SERVICE_NAME` from a constant —
    // it doesn't read `Config.service.name`. We set it here anyway so the
    // test runs in a configuration that matches `serve.rs`.
    let mut config = Config::<SchemaForgeConfig>::default();
    config.service.name = SCHEMA_FORGE_SERVICE_NAME.to_string();
    let state = acton_service::state::AppState::<SchemaForgeConfig>::new(config);

    stateful.with_state(state)
}

/// Fetch a path and return (status, parsed JSON body).
async fn get_json(app: &Router, path: &str) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("build request");
    let response = app.clone().oneshot(request).await.expect("oneshot");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_reports_schema_forge_acton_version() {
    let app = build_router().await;
    let (status, body) = get_json(&app, "/health").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["version"].as_str(),
        Some(SCHEMA_FORGE_ACTON_VERSION),
        "/health must report schema-forge-acton's CARGO_PKG_VERSION, not \
         acton-service's; body: {body}"
    );
    assert_eq!(
        body["service"].as_str(),
        Some(SCHEMA_FORGE_SERVICE_NAME),
        "service name should match SCHEMA_FORGE_SERVICE_NAME; body: {body}"
    );
    assert_eq!(body["status"].as_str(), Some("healthy"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_and_meta_agree_on_version() {
    let app = build_router().await;

    let (health_status, health) = get_json(&app, "/health").await;
    let (meta_status, meta) = get_json(&app, "/api/v1/forge/meta").await;

    assert_eq!(health_status, StatusCode::OK, "health body: {health}");
    assert_eq!(meta_status, StatusCode::OK, "meta body: {meta}");

    let health_version = health["version"]
        .as_str()
        .expect("/health version field missing");
    let meta_version = meta["build"]["version"]
        .as_str()
        .expect("/meta build.version field missing");

    assert_eq!(
        health_version, SCHEMA_FORGE_ACTON_VERSION,
        "/health version drift; body: {health}"
    );
    assert_eq!(
        meta_version, SCHEMA_FORGE_ACTON_VERSION,
        "/meta version drift; body: {meta}"
    );
    assert_eq!(
        health_version, meta_version,
        "/health and /meta must agree on version (issue #55). \
         health={health_version}, meta={meta_version}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_endpoint_still_routes_through_acton_service() {
    // The middleware should only intercept GET /health. /ready must still
    // reach acton-service's readiness handler.
    let app = build_router().await;
    let request = Request::builder()
        .method("GET")
        .uri("/ready")
        .body(Body::empty())
        .expect("build request");
    let response = app.oneshot(request).await.expect("oneshot");
    // Without any database/cache features wired into this minimal harness,
    // /ready returns 200 with an empty `dependencies` map — but the key
    // assertion is that it doesn't 404, which would mean our middleware
    // swallowed it.
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "/ready was swallowed by the health middleware"
    );
}
