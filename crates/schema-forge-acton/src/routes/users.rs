//! User management endpoints under `/api/v1/forge/users`.
//!
//! Restores the legacy HTMX `/admin/users` CRUD surface on the new REST +
//! React admin stack. Routes authenticate via the upstream acton-service
//! token middleware (injecting `Claims`) and pull the `AuthStore` off an
//! `Extension<Arc<dyn DynAuthStore>>` layer that `build_versioned_routes`
//! already attaches to every `/api/v1/*` route.
//!
//! Authorization model:
//! - `GET /users`, `POST /users`, `DELETE /users/:username` require the
//!   `admin` role.
//! - `POST /users/:username/password` allows admin OR self (the token's
//!   `sub` matches `user:<username>` or the bare username).
//!
//! Duplicate usernames are rejected up front via `AuthStore::get_user`
//! since neither backend surfaces a typed conflict error (both just
//! propagate the DB unique-constraint error as `QueryError`). This keeps
//! `ForgeError` untouched — the caller sees a `validation_failed`
//! envelope with a clear message.

use std::sync::Arc;

use acton_service::middleware::Claims;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use schema_forge_backend::user_store::ForgeUser;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::access::OptionalClaims;
use crate::error::ForgeError;
use crate::state::DynAuthStore;

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Require authentication. Returns 401 if no Claims present.
///
/// Duplicated (by design) from `routes::schemas` because that file is owned
/// by a parallel agent and cannot be edited. The helper is trivially pure
/// and unit-tested below.
fn require_auth(claims: &Option<Claims>) -> Result<&Claims, ForgeError> {
    claims.as_ref().ok_or(ForgeError::Unauthorized {
        message: "authentication required".to_string(),
    })
}

/// Require the admin role. Returns 403 if the user lacks it.
fn require_admin(claims: &Claims) -> Result<(), ForgeError> {
    if claims.has_role("admin") {
        Ok(())
    } else {
        Err(ForgeError::Forbidden {
            message: "user management requires admin role".to_string(),
        })
    }
}

/// Require admin role OR that the caller's token subject names `target`.
///
/// The login handler emits `sub = "user:<username>"`; we also accept a
/// bare username to stay resilient to alternative claim sources.
fn require_admin_or_self(claims: &Claims, target: &str) -> Result<(), ForgeError> {
    if claims.has_role("admin") {
        return Ok(());
    }
    let prefixed = format!("user:{target}");
    if claims.sub == prefixed || claims.sub == target {
        return Ok(());
    }
    Err(ForgeError::Forbidden {
        message: "admin role or self required".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Validation helpers (pure, unit-testable)
// ---------------------------------------------------------------------------

const MAX_USERNAME_LEN: usize = 64;
const MIN_PASSWORD_LEN: usize = 8;

/// Verify a username conforms to the legacy charset `[A-Za-z0-9_.-]{1,64}`.
fn validate_username(username: &str) -> Result<(), ForgeError> {
    if username.is_empty() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["username must not be empty".to_string()],
        });
    }
    if username.len() > MAX_USERNAME_LEN {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "username exceeds maximum length of {MAX_USERNAME_LEN}"
            )],
        });
    }
    let all_ok = username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !all_ok {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "username '{username}' contains invalid characters \
                 (allowed: letters, digits, '_', '.', '-')"
            )],
        });
    }
    Ok(())
}

/// Verify a plaintext password meets the minimum length requirement.
fn validate_password(password: &str) -> Result<(), ForgeError> {
    if password.is_empty() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["password must not be empty".to_string()],
        });
    }
    if password.len() < MIN_PASSWORD_LEN {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "password must be at least {MIN_PASSWORD_LEN} characters long"
            )],
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// Response body for a single user row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserResponse {
    /// Unique username.
    pub username: String,
    /// Role tags attached to the user.
    pub roles: Vec<String>,
    /// Optional display name.
    pub display_name: Option<String>,
    /// Whether the user is currently allowed to log in.
    pub active: bool,
}

/// Response body for `GET /users`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListUsersResponse {
    /// Users, ordered as the store returned them.
    pub users: Vec<UserResponse>,
    /// Total count of users in the store.
    pub count: usize,
}

/// Request body for `POST /users`.
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    /// Username to create.
    pub username: String,
    /// Plaintext password; will be hashed by the backend.
    pub password: String,
    /// Role tags to assign. Defaults to an empty list.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Optional display name. Defaults to the username when absent.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Request body for `POST /users/:username/password`.
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    /// New plaintext password; will be hashed by the backend.
    pub password: String,
}

/// Pure helper: project a `ForgeUser` into the wire response shape.
fn user_to_response(user: &ForgeUser) -> UserResponse {
    UserResponse {
        username: user.username.clone(),
        roles: user.roles.clone(),
        display_name: user.display_name.clone(),
        active: user.active,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /users` — list every user. Admin only.
#[instrument(skip_all)]
pub async fn list_users(
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    require_admin(claims)?;

    let users = auth_store.list_users().await?;
    let responses: Vec<UserResponse> = users.iter().map(user_to_response).collect();
    let count = responses.len();
    Ok(Json(ListUsersResponse {
        users: responses,
        count,
    }))
}

/// `POST /users` — create a new user. Admin only.
#[instrument(skip_all)]
pub async fn create_user(
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<CreateUserRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    require_admin(claims)?;

    validate_username(&body.username)?;
    validate_password(&body.password)?;

    // Pre-check to surface duplicates as 422 instead of a raw backend error.
    if auth_store.get_user(&body.username).await?.is_some() {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!("user '{}' already exists", body.username)],
        });
    }

    let display_name = body
        .display_name
        .clone()
        .unwrap_or_else(|| body.username.clone());

    auth_store
        .create_user(&body.username, &body.password, &body.roles, &display_name)
        .await?;

    let created =
        auth_store
            .get_user(&body.username)
            .await?
            .ok_or_else(|| ForgeError::Internal {
                message: format!("created user '{}' not found on readback", body.username),
            })?;

    Ok((StatusCode::CREATED, Json(user_to_response(&created))))
}

/// `DELETE /users/:username` — delete a user. Admin only.
///
/// Refuses to delete the caller themselves as a defense-in-depth against an
/// admin locking themselves out mid-session.
#[instrument(skip_all)]
pub async fn delete_user(
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Path(username): Path<String>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    require_admin(claims)?;

    let prefixed = format!("user:{username}");
    if claims.sub == prefixed || claims.sub == username {
        return Err(ForgeError::ValidationFailed {
            details: vec!["cannot delete yourself".to_string()],
        });
    }

    if auth_store.get_user(&username).await?.is_none() {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!("user '{username}' not found")],
        });
    }

    auth_store.delete_user(&username).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /users/:username/password` — change a user's password.
///
/// Allowed when the caller has the admin role OR when the caller's token
/// subject matches `username`.
#[instrument(skip_all)]
pub async fn change_password(
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Path(username): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    require_admin_or_self(claims, &username)?;

    validate_password(&body.password)?;

    if auth_store.get_user(&username).await?.is_none() {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!("user '{username}' not found")],
        });
    }

    auth_store
        .change_password(&username, &body.password)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Unit tests for pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn claims_with_sub(sub: &str, roles: &[&str]) -> Claims {
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

    #[test]
    fn validate_username_accepts_ascii_names() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("bob.smith").is_ok());
        assert!(validate_username("ci-runner_01").is_ok());
    }

    #[test]
    fn validate_username_rejects_empty() {
        assert!(validate_username("").is_err());
    }

    #[test]
    fn validate_username_rejects_special_chars() {
        assert!(validate_username("a b").is_err());
        assert!(validate_username("alice@example.com").is_err());
    }

    #[test]
    fn validate_username_rejects_too_long() {
        let name: String = "a".repeat(MAX_USERNAME_LEN + 1);
        assert!(validate_username(&name).is_err());
    }

    #[test]
    fn validate_password_rejects_empty() {
        assert!(validate_password("").is_err());
    }

    #[test]
    fn validate_password_rejects_too_short() {
        assert!(validate_password("short").is_err());
    }

    #[test]
    fn validate_password_accepts_minimum_length() {
        assert!(validate_password("abcdefgh").is_ok());
    }

    #[test]
    fn user_to_response_copies_all_fields() {
        let src = ForgeUser {
            username: "alice".to_string(),
            roles: vec!["admin".to_string(), "hr".to_string()],
            display_name: Some("Alice".to_string()),
            active: true,
        };
        let out = user_to_response(&src);
        assert_eq!(out.username, "alice");
        assert_eq!(out.roles, vec!["admin".to_string(), "hr".to_string()]);
        assert_eq!(out.display_name.as_deref(), Some("Alice"));
        assert!(out.active);
    }

    #[test]
    fn require_admin_or_self_allows_admin() {
        let c = claims_with_sub("user:carol", &["admin"]);
        assert!(require_admin_or_self(&c, "alice").is_ok());
    }

    #[test]
    fn require_admin_or_self_allows_self_prefixed_sub() {
        let c = claims_with_sub("user:alice", &[]);
        assert!(require_admin_or_self(&c, "alice").is_ok());
    }

    #[test]
    fn require_admin_or_self_allows_self_bare_sub() {
        let c = claims_with_sub("alice", &[]);
        assert!(require_admin_or_self(&c, "alice").is_ok());
    }

    #[test]
    fn require_admin_or_self_rejects_other_user() {
        let c = claims_with_sub("user:bob", &["member"]);
        let err = require_admin_or_self(&c, "alice").unwrap_err();
        assert!(matches!(err, ForgeError::Forbidden { .. }));
    }

    #[test]
    fn require_auth_returns_unauthorized_when_missing() {
        let err = require_auth(&None).unwrap_err();
        assert!(matches!(err, ForgeError::Unauthorized { .. }));
    }

    #[test]
    fn require_admin_rejects_non_admin() {
        let c = claims_with_sub("user:alice", &["member"]);
        assert!(require_admin(&c).is_err());
    }
}
