//! SchemaForge-owned `/health` override.
//!
//! # Why this exists
//!
//! `acton-service` mounts its own `/health` route whose handler reports
//! `env!("CARGO_PKG_VERSION")` of the **`acton-service` crate** — not of the
//! running SchemaForge binary. That number lags every time SchemaForge cuts a
//! release without bumping the upstream `acton-service` dependency, and it
//! disagrees with `GET /api/v1/forge/meta`'s `build.version` (which already
//! reports the running `schema-forge-acton` crate version).
//!
//! `acton-service 0.23` exposes no public hook for overriding the version
//! string. Until upstream gains one, SchemaForge installs an axum middleware
//! that intercepts `GET /health` *before* acton-service's handler runs and
//! returns a wire-compatible response carrying this crate's
//! `CARGO_PKG_VERSION`.
//!
//! # Removal criteria
//!
//! Drop this middleware once `acton-service` accepts a service-version
//! override (e.g. through `Config.service.version` or a builder method).
//! Tracked as SchemaForge issue #55.

use axum::Json;
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// `schema-forge-acton` crate version, stamped at compile time.
///
/// This is the version `/health` reports and is the same value
/// `MetaBuild::version` uses for `/api/v1/forge/meta`. Keeping a single
/// `env!` call here makes the two endpoints provably consistent.
pub const SCHEMA_FORGE_ACTON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire-compatible mirror of `acton_service::health::HealthResponse`.
///
/// Field order and names match acton-service exactly so existing clients
/// (k8s liveness probes, monitoring dashboards) keep working without
/// changes. The only difference is that `version` here is the running
/// `schema-forge-acton` crate's `CARGO_PKG_VERSION`, not acton-service's.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Always `"healthy"` while this handler is reachable (liveness probe).
    pub status: &'static str,
    /// Service name from `Config.service.name`. SchemaForge sets this to
    /// `"schemaforge"` in `serve.rs`.
    pub service: String,
    /// Running `schema-forge-acton` crate version — matches
    /// `/api/v1/forge/meta`'s `build.version` and the CLI `--version` (when
    /// they release in lockstep).
    pub version: &'static str,
}

/// Service name reported by [`schema_forge_health_middleware`].
///
/// Matches the value `schema-forge-cli`'s `serve.rs` writes into
/// `Config.service.name` (`svc_config.service.name = "schemaforge"`). The
/// constant is exposed publicly so callers can assert against it without
/// duplicating the literal.
pub const SCHEMA_FORGE_SERVICE_NAME: &str = "schemaforge";

/// Axum middleware that intercepts `GET /health` and replies with
/// SchemaForge's [`HealthResponse`].
///
/// All other requests pass through to the inner router (notably
/// acton-service's `/ready` handler and the nested `/api/v1/...` routes).
/// The middleware reports [`SCHEMA_FORGE_SERVICE_NAME`] as `service` in
/// the body and [`SCHEMA_FORGE_ACTON_VERSION`] as `version`.
///
/// Wired by `schema-forge-cli`'s `serve` command via
/// `Router::layer(axum::middleware::from_fn(...))` on the `VersionedRoutes`
/// inner router. The middleware short-circuits before acton-service's
/// `/health` handler is dispatched, so there is no method-route conflict.
///
/// The service name is a compile-time constant rather than being extracted
/// from `AppState` so the middleware stays independent of any config type
/// `T` and can be wired with the plain `axum::middleware::from_fn` (no
/// state binding required). `serve.rs` already hard-codes the same string
/// into `Config.service.name`, so the two are kept in lockstep via
/// [`SCHEMA_FORGE_SERVICE_NAME`].
pub async fn schema_forge_health_middleware(req: Request, next: Next) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/health" {
        let body = HealthResponse {
            status: "healthy",
            service: SCHEMA_FORGE_SERVICE_NAME.to_string(),
            version: SCHEMA_FORGE_ACTON_VERSION,
        };
        return (StatusCode::OK, Json(body)).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constant_matches_meta_build_version() {
        // Both endpoints derive their version from the same crate-level
        // env!("CARGO_PKG_VERSION"). This guards against someone wiring
        // /health to a different constant by accident.
        let meta = crate::routes::MetaInfo::new("surrealdb", "test", 60);
        assert_eq!(meta.build.version, SCHEMA_FORGE_ACTON_VERSION);
    }

    #[test]
    fn health_response_serializes_with_acton_compatible_shape() {
        let body = HealthResponse {
            status: "healthy",
            service: "schemaforge".to_string(),
            version: "9.9.9",
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["service"], "schemaforge");
        assert_eq!(json["version"], "9.9.9");
    }

    #[test]
    fn service_name_constant_matches_serve_rs() {
        // serve.rs sets `svc_config.service.name = "schemaforge"`. Keep
        // these two in sync via this constant.
        assert_eq!(SCHEMA_FORGE_SERVICE_NAME, "schemaforge");
    }
}
