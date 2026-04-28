//! JSON login endpoint: `POST /api/v1/forge/auth/login`.
//!
//! Validates credentials against the configured `AuthStore` (the same store
//! the HTMX site login uses) and mints a PASETO V4 token via
//! [`acton_service::auth::tokens::paseto_generator::PasetoGenerator`]. The
//! resulting token can be presented as a Bearer credential to the rest of
//! the SchemaForge API.
//!
//! The generator and auth store are provided to the handler as axum
//! `Extension`s; `commands::serve` constructs both once at boot and layers
//! them onto the versioned router.

use std::sync::Arc;
use std::time::Duration;

use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::auth::tokens::{ClaimsBuilder, TokenGenerator};
use acton_service::middleware::Claims;
use acton_service::prelude::Error as ActonError;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::access::OptionalClaims;
use crate::state::DynAuthStore;

/// Default expiry for tokens minted by this endpoint (1 hour).
///
/// The task spec calls for 1 hour explicitly; the acton-service default of
/// 15 minutes is too short for the generated React site's current UX.
///
/// Public so `/meta` can surface the live TTL without duplicating the value.
pub const LOGIN_TOKEN_LIFETIME: Duration = Duration::from_secs(3600);

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Request body for `POST /auth/login`.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Username to authenticate.
    pub username: String,
    /// Plaintext password (HTTPS-terminated by the deployment front door).
    pub password: String,
}

/// Success response body for `POST /auth/login`.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The minted PASETO V4 token (prefixed `v4.local.` or `v4.public.`).
    pub token: String,
    /// ISO-8601 UTC timestamp at which the token expires.
    pub expires_at: String,
    /// Roles granted to the authenticated user. The generated React site
    /// stashes this alongside the token so `@field_access` annotations can
    /// be enforced client-side without decoding the opaque PASETO payload.
    pub roles: Vec<String>,
}

/// Error envelope for 401 responses.
///
/// Shape intentionally matches the task spec (not the generic `ForgeError`
/// envelope) so API clients can detect the login endpoint's failure mode
/// without parsing free-text error kinds.
#[derive(Debug, Serialize)]
struct LoginErrorBody {
    error: &'static str,
    code: &'static str,
    status: u16,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `POST /auth/login` — validate credentials and mint a PASETO token.
///
/// Flow:
/// 1. Look up the supplied username in the auth store.
/// 2. Argon2-verify the password (delegated to `AuthStore::validate_credentials`).
/// 3. Build claims: `sub = "user:<username>"`, roles from the stored user,
///    `username` set on the claims for downstream loggers. This mirrors the
///    shape produced by the session-to-claims bridge in `widget::auth`.
/// 4. Generate a PASETO token with a 1-hour expiry.
///
/// Internal errors (auth store unavailable, token generation failure) are
/// surfaced as 500 so they are easy to distinguish from the 401 "bad
/// credentials" case.
pub async fn login(
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Extension(generator): Extension<Arc<PasetoGenerator>>,
    Json(req): Json<LoginRequest>,
) -> Response {
    let user = match auth_store
        .validate_credentials(&req.username, &req.password)
        .await
    {
        Ok(Some(u)) => u,
        Ok(None) => return unauthorized_response(),
        Err(e) => return internal_error_response(format!("auth store error: {e}")),
    };

    let claims = match build_login_claims(&user.username, &user.roles) {
        Ok(c) => c,
        Err(e) => return internal_error_response(format!("failed to build claims: {e}")),
    };

    let token = match generator.generate_token_with_expiry(&claims, LOGIN_TOKEN_LIFETIME) {
        Ok(t) => t,
        Err(e) => return internal_error_response(format!("failed to generate token: {e}")),
    };

    let expires_at = Utc::now() + chrono::Duration::seconds(LOGIN_TOKEN_LIFETIME.as_secs() as i64);

    let body = LoginResponse {
        token,
        expires_at: expires_at.to_rfc3339(),
        roles: user.roles.clone(),
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// `POST /auth/refresh` — exchange a still-valid bearer token for a fresh
/// one with a new 1-hour expiry.
///
/// The acton-service auth middleware already validates the incoming bearer
/// and makes it available as an `Extension<Claims>`; we only have to mint a
/// replacement token carrying the same identity and roles. Callers with an
/// expired or missing token get a clean 401 — the client can then show the
/// login screen without hitting a ginned-up internal error.
pub async fn refresh(
    OptionalClaims(claims): OptionalClaims,
    Extension(generator): Extension<Arc<PasetoGenerator>>,
) -> Response {
    let Some(claims) = claims else {
        return unauthorized_response();
    };

    let username = claims
        .username
        .clone()
        .unwrap_or_else(|| claims.sub.trim_start_matches("user:").to_string());

    let next_claims = match build_login_claims(&username, &claims.roles) {
        Ok(c) => c,
        Err(e) => return internal_error_response(format!("failed to build claims: {e}")),
    };

    let token = match generator.generate_token_with_expiry(&next_claims, LOGIN_TOKEN_LIFETIME) {
        Ok(t) => t,
        Err(e) => return internal_error_response(format!("failed to generate token: {e}")),
    };

    let expires_at = Utc::now() + chrono::Duration::seconds(LOGIN_TOKEN_LIFETIME.as_secs() as i64);

    let body = LoginResponse {
        token,
        expires_at: expires_at.to_rfc3339(),
        roles: claims.roles.clone(),
    };
    (StatusCode::OK, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable)
// ---------------------------------------------------------------------------

/// Build the PASETO claims for a successful login.
///
/// Pure function: no I/O, no state. Used by the handler and by the unit
/// tests that exercise the claim shape without needing a live generator.
pub(crate) fn build_login_claims(username: &str, roles: &[String]) -> Result<Claims, ActonError> {
    let mut builder = ClaimsBuilder::new().user(username).username(username);
    for role in roles {
        builder = builder.role(role);
    }
    builder = builder.issuer("schemaforge");
    builder.build()
}

fn unauthorized_response() -> Response {
    let body = LoginErrorBody {
        error: "invalid credentials",
        code: "UNAUTHORIZED",
        status: 401,
    };
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

fn internal_error_response(message: String) -> Response {
    tracing::error!(error = %message, "login endpoint internal error");
    let body = serde_json::json!({
        "error": "internal_error",
        "message": message,
    });
    (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Router helper
// ---------------------------------------------------------------------------

/// Build the auth sub-router, containing just `POST /auth/login`.
///
/// Nested alongside the rest of the forge routes (schemas, entities) under
/// `/api/v1/forge/` by `SchemaForgeExtension::versioned_forge_routes`.
pub fn auth_routes(
) -> axum::Router<acton_service::state::AppState<crate::config::SchemaForgeConfig>> {
    use axum::routing::post;
    axum::Router::new()
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_login_claims_sets_subject_and_roles() {
        let claims = build_login_claims("alice", &["admin".to_string(), "hr".to_string()]).unwrap();
        assert_eq!(claims.sub, "user:alice");
        assert_eq!(claims.roles, vec!["admin".to_string(), "hr".to_string()]);
        assert_eq!(claims.username.as_deref(), Some("alice"));
        assert_eq!(claims.iss.as_deref(), Some("schemaforge"));
    }

    #[test]
    fn build_login_claims_with_no_roles() {
        let claims = build_login_claims("bob", &[]).unwrap();
        assert_eq!(claims.sub, "user:bob");
        assert!(claims.roles.is_empty());
    }
}
