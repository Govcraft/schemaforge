//! Public meta endpoint: `GET /api/v1/forge/meta`.
//!
//! Returns the runtime posture (backend kind, auth scheme + TTL, build
//! version) so unauthenticated surfaces — the login screen in particular —
//! can show real values instead of hardcoded ones. The endpoint is
//! intentionally public and cheap: no auth, no DB round-trip, no actor
//! dispatch — just an `Extension<MetaInfo>` constructed once at startup.
//!
//! `serve.rs` builds the `MetaInfo` from the resolved `DbParams` and the
//! login token lifetime, and layers it onto the versioned router.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Serialize;

/// Runtime posture surfaced through `/meta`.
///
/// Constructed once by the binary at startup and shared via `Arc` so the
/// HTTP layer can extract it cheaply on every request.
#[derive(Debug, Clone, Serialize)]
pub struct MetaInfo {
    /// Backend identifier — one of `"surrealdb"`, `"postgres"`, `"turso"`.
    /// Lowercase, stable: API clients should match on this token, not the
    /// human label below.
    pub backend: &'static str,
    /// Backend label suitable for human display (`"SurrealDB 2.x"`).
    pub backend_label: String,
    /// Auth posture (always PASETO V4 today).
    pub auth: MetaAuth,
    /// Build/version posture for the running schema-forge-acton.
    pub build: MetaBuild,
}

/// Auth subsection of [`MetaInfo`].
#[derive(Debug, Clone, Serialize)]
pub struct MetaAuth {
    /// Token kind. `"paseto"` today; reserved for future schemes.
    pub kind: &'static str,
    /// Login token lifetime in seconds. The login screen formats this as
    /// `60m TTL` etc.
    pub ttl_seconds: u64,
}

/// Build subsection of [`MetaInfo`].
#[derive(Debug, Clone, Serialize)]
pub struct MetaBuild {
    /// Cargo package version of the running schema-forge-acton crate
    /// (`env!("CARGO_PKG_VERSION")`). The CLI's version may differ; the
    /// runtime value is what's most actionable for incident triage.
    pub version: &'static str,
}

impl MetaInfo {
    /// Build a `MetaInfo` snapshot. `backend` and `backend_label` are
    /// caller-supplied so the binary that knows the resolved DB scheme can
    /// pass it in without dragging the schema-forge-cli config types into
    /// this crate.
    pub fn new(backend: &'static str, backend_label: impl Into<String>, ttl_seconds: u64) -> Self {
        Self {
            backend,
            backend_label: backend_label.into(),
            auth: MetaAuth {
                kind: "paseto",
                ttl_seconds,
            },
            build: MetaBuild {
                version: env!("CARGO_PKG_VERSION"),
            },
        }
    }
}

/// `GET /meta` — return the cached `MetaInfo` snapshot.
///
/// Always 200 once mounted; the only reason this would fail is if a binary
/// forgot to layer `Extension(Arc<MetaInfo>)`, which is a programmer error
/// surfaced as 500 rather than a silent default.
pub async fn get_meta(meta: Option<Extension<Arc<MetaInfo>>>) -> Response {
    match meta {
        Some(Extension(info)) => {
            let mut body = serde_json::json!(info.as_ref());
            body["capabilities"] = serde_json::json!({
                "create_reconciliation": {
                    "protocol": "create-intent-v1",
                    "backend_supported": info.backend == "postgres",
                    "request_header": "Create-Intent",
                    "schema_support": "hook-free",
                    "admission_seconds": 900,
                    "recovery_seconds": 86400
                },
                "conditional_entity_mutations": {
                    "protocol": "record-revision-v1",
                    "backend_supported": info.backend == "postgres",
                    "schema_readiness": "entity-revision-response-header",
                    "request_header": "If-Entity-Revision",
                    "response_header": "Entity-Revision"
                }
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        None => {
            let body = serde_json::json!({
                "error": "meta endpoint not configured",
                "code": "META_UNAVAILABLE",
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

/// Build the meta sub-router, containing just `GET /meta`.
///
/// Merged alongside `auth_routes()` under `/api/v1/forge/` so the login
/// screen can hit it without any auth context.
pub fn meta_routes(
) -> axum::Router<acton_service::state::AppState<crate::config::SchemaForgeConfig>> {
    use axum::routing::get;
    axum::Router::new().route("/meta", get(get_meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_info_carries_compile_time_version() {
        let info = MetaInfo::new("surrealdb", "SurrealDB 2.x", 3600);
        assert_eq!(info.backend, "surrealdb");
        assert_eq!(info.backend_label, "SurrealDB 2.x");
        assert_eq!(info.auth.kind, "paseto");
        assert_eq!(info.auth.ttl_seconds, 3600);
        // The build version is whatever Cargo stamped on this crate.
        assert!(!info.build.version.is_empty());
    }

    #[tokio::test]
    async fn metadata_distinguishes_adapter_support_from_schema_readiness() {
        for (backend, supported) in [("postgres", true), ("surrealdb", false), ("mssql", false)] {
            let response = get_meta(Some(Extension(Arc::new(MetaInfo::new(
                backend, backend, 3600,
            )))))
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let capability = &body["capabilities"]["conditional_entity_mutations"];
            assert_eq!(capability["backend_supported"], supported);
            assert_eq!(capability["protocol"], "record-revision-v1");
            assert_eq!(
                capability["schema_readiness"],
                "entity-revision-response-header"
            );
            assert_eq!(body["backend"], backend);
        }
    }
}
