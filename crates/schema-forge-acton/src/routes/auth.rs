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

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use acton_service::audit::{AuditEventKind, AuditSeverity, AuditSource};
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::auth::tokens::{ClaimsBuilder, TokenGenerator};
use acton_service::middleware::Claims;
use acton_service::prelude::Error as ActonError;
use acton_service::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use schema_forge_backend::tenant::{TenantConfig, TenantRef};
use schema_forge_backend::Entity;
use serde::{Deserialize, Serialize};

use crate::access::{OptionalClaims, PLATFORM_ADMIN_ROLE};
use crate::authz::principal_claims::{PrincipalClaimMappings, PrincipalClaimsError};
use crate::config::SchemaForgeConfig;
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
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Extension(generator): Extension<Arc<PasetoGenerator>>,
    Extension(principal_claims): Extension<Arc<PrincipalClaimMappings>>,
    Extension(tenant_config): Extension<Arc<Option<TenantConfig>>>,
    Json(req): Json<LoginRequest>,
) -> Response {
    let user = match auth_store
        .validate_credentials(&req.username, &req.password)
        .await
    {
        Ok(Some(u)) => u,
        Ok(None) => {
            emit_login_failed(&state, &req.username).await;
            return unauthorized_response();
        }
        Err(e) => {
            emit_login_failed(&state, &req.username).await;
            return internal_error_response(format!("auth store error: {e}"));
        }
    };

    let user_entity = if principal_claims.has_user_field_sources() {
        match auth_store.get_user_entity(&user.username).await {
            Ok(Some(e)) => Some(e),
            Ok(None) => {
                emit_login_failed(&state, &req.username).await;
                return unauthorized_response();
            }
            Err(e) => return internal_error_response(format!("auth store error: {e}")),
        }
    } else {
        None
    };

    let memberships = match auth_store.list_tenant_memberships(&user.username).await {
        Ok(m) => m,
        Err(e) => return internal_error_response(format!("auth store error: {e}")),
    };

    if let Err(refusal) = enforce_tenant_membership_policy(
        &memberships,
        &user.roles,
        tenant_config.as_ref().as_ref(),
    ) {
        emit_login_failed(&state, &req.username).await;
        return refusal.into_response();
    }

    let claims = match build_login_claims(
        &user.username,
        &user.roles,
        user_entity.as_ref(),
        &principal_claims,
        &memberships,
    ) {
        Ok(c) => c,
        Err(BuildLoginClaimsError::NullRequired(_)) => {
            emit_login_failed(&state, &req.username).await;
            return unauthorized_response();
        }
        Err(e) => return internal_error_response(format!("failed to build claims: {e}")),
    };

    let token = match generator.generate_token_with_expiry(&claims, LOGIN_TOKEN_LIFETIME) {
        Ok(t) => t,
        Err(e) => return internal_error_response(format!("failed to generate token: {e}")),
    };

    let issued_at = Utc::now();

    // Persist before returning the token. Fails closed: a DB write failure
    // produces a 500 rather than handing the caller a token without an
    // audit trail entry. The login handler already proved the row exists
    // via validate_credentials, so the only realistic failure mode is a
    // backend outage — in which case 500 is the right answer.
    if let Err(e) = auth_store.record_login(&user.username, issued_at).await {
        return internal_error_response(format!("failed to record last_login: {e}"));
    }

    emit_login_success(&state, &user.username).await;

    let expires_at = issued_at + chrono::Duration::seconds(LOGIN_TOKEN_LIFETIME.as_secs() as i64);

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
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Extension(generator): Extension<Arc<PasetoGenerator>>,
    Extension(principal_claims): Extension<Arc<PrincipalClaimMappings>>,
    Extension(tenant_config): Extension<Arc<Option<TenantConfig>>>,
) -> Response {
    let Some(claims) = claims else {
        return unauthorized_response();
    };

    let username = claims
        .username
        .clone()
        .unwrap_or_else(|| claims.sub.trim_start_matches("user:").to_string());

    // Re-read the User row on every refresh — no claim copy-forward. A row
    // mutated since the original login (e.g., role change, client_org
    // reassignment) takes effect immediately on the next refresh, which is
    // load-bearing for per-record scoping that depends on `principal.*`
    // attributes derived from User columns. Same contract for memberships
    // below: a grant or revocation lands on the next refresh.
    let user = match auth_store.get_user(&username).await {
        Ok(Some(u)) if u.active => u,
        Ok(_) => return unauthorized_response(),
        Err(e) => return internal_error_response(format!("auth store error: {e}")),
    };

    let user_entity = if principal_claims.has_user_field_sources() {
        match auth_store.get_user_entity(&username).await {
            Ok(Some(e)) => Some(e),
            Ok(None) => return unauthorized_response(),
            Err(e) => return internal_error_response(format!("auth store error: {e}")),
        }
    } else {
        None
    };

    let memberships = match auth_store.list_tenant_memberships(&user.username).await {
        Ok(m) => m,
        Err(e) => return internal_error_response(format!("auth store error: {e}")),
    };

    if let Err(refusal) = enforce_tenant_membership_policy(
        &memberships,
        &user.roles,
        tenant_config.as_ref().as_ref(),
    ) {
        return refusal.into_response();
    }

    let next_claims = match build_login_claims(
        &user.username,
        &user.roles,
        user_entity.as_ref(),
        &principal_claims,
        &memberships,
    ) {
        Ok(c) => c,
        Err(BuildLoginClaimsError::NullRequired(_)) => return unauthorized_response(),
        Err(e) => return internal_error_response(format!("failed to build claims: {e}")),
    };

    let token = match generator.generate_token_with_expiry(&next_claims, LOGIN_TOKEN_LIFETIME) {
        Ok(t) => t,
        Err(e) => return internal_error_response(format!("failed to generate token: {e}")),
    };

    let expires_at = Utc::now() + chrono::Duration::seconds(LOGIN_TOKEN_LIFETIME.as_secs() as i64);

    emit_token_refresh(&state, &user.username).await;

    let body = LoginResponse {
        token,
        expires_at: expires_at.to_rfc3339(),
        roles: user.roles.clone(),
    };
    (StatusCode::OK, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Audit emission helpers
// ---------------------------------------------------------------------------

async fn emit_login_success(state: &AppState<SchemaForgeConfig>, username: &str) {
    if let Some(logger) = state.audit_logger() {
        logger
            .log_auth(
                AuditEventKind::AuthLoginSuccess,
                AuditSeverity::Informational,
                AuditSource {
                    subject: Some(format!("user:{username}")),
                    ..AuditSource::default()
                },
            )
            .await;
    }
}

async fn emit_login_failed(state: &AppState<SchemaForgeConfig>, attempted_username: &str) {
    if let Some(logger) = state.audit_logger() {
        logger
            .log_auth(
                AuditEventKind::AuthLoginFailed,
                AuditSeverity::Warning,
                AuditSource {
                    subject: Some(format!("user:{attempted_username}")),
                    ..AuditSource::default()
                },
            )
            .await;
    }
}

async fn emit_token_refresh(state: &AppState<SchemaForgeConfig>, username: &str) {
    if let Some(logger) = state.audit_logger() {
        logger
            .log_auth(
                AuditEventKind::AuthTokenRefresh,
                AuditSeverity::Informational,
                AuditSource {
                    subject: Some(format!("user:{username}")),
                    ..AuditSource::default()
                },
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable)
// ---------------------------------------------------------------------------

/// Build the PASETO claims for a successful login.
///
/// Pure function: no I/O, no state. Sets the standard subject/username/role
/// claims and projects every operator-declared `source = { user_field = ... }`
/// principal-claim mapping into the PASETO `custom` map.
///
/// Returns [`BuildLoginClaimsError::NullRequired`] when a `required`
/// principal-claim's user-field is null/missing — the caller maps this to a
/// 401 response with the standard invalid-credentials envelope.
pub(crate) fn build_login_claims(
    username: &str,
    roles: &[String],
    user_entity: Option<&Entity>,
    principal_claims: &PrincipalClaimMappings,
    memberships: &[TenantRef],
) -> Result<Claims, BuildLoginClaimsError> {
    let mut builder = ClaimsBuilder::new().user(username).username(username);
    for role in roles {
        builder = builder.role(role);
    }
    builder = builder.issuer("schemaforge");

    if principal_claims.has_user_field_sources() {
        let entity = user_entity.ok_or(BuildLoginClaimsError::MissingUserEntity)?;
        let projected = principal_claims
            .project_user_fields(entity)
            .map_err(|e| match e {
                PrincipalClaimsError::NullRequiredUserField { .. } => {
                    BuildLoginClaimsError::NullRequired(e)
                }
                other => BuildLoginClaimsError::PrincipalClaim(other),
            })?;
        for (k, v) in projected {
            builder = builder.custom_claim(k, v);
        }
    }

    // Project the user's flat tenant-membership set into the token. The
    // claim name stays `tenant_chain` for backward compatibility with the
    // existing Cedar adapter and access.rs reads, but it now carries the
    // user's full membership set — *not* a hierarchy walk. The active
    // tenant + ancestor chain is materialized per-request by the
    // `tenant_scope` middleware before downstream handlers see Claims.
    if !memberships.is_empty() {
        let value = serde_json::to_value(memberships)
            .map_err(BuildLoginClaimsError::MembershipSerialize)?;
        builder = builder.custom_claim("tenant_chain", value);
    }

    builder.build().map_err(BuildLoginClaimsError::Acton)
}

/// Decide whether login should proceed given the user's membership set,
/// roles, and the deployment's tenant configuration.
///
/// Rules:
/// - Tenancy not configured (`tenant_config` is `None` or disabled) → always
///   `Ok(())`. Single-tenant deployments are unaffected.
/// - `platform_admin` in roles → always `Ok(())`. The platform-admin role
///   transcends tenancy; this matches the existing access.rs bypass.
/// - 0 memberships under enabled tenancy → `Err(NoTenantAssigned)`. Closes
///   the "user logs in and sees every tenant" hole hard. Operators must
///   explicitly grant a `TenantMembership` row before the user can hit the
///   API.
/// - 1+ memberships → `Ok(())`.
pub(crate) fn enforce_tenant_membership_policy(
    memberships: &[TenantRef],
    roles: &[String],
    tenant_config: Option<&TenantConfig>,
) -> Result<(), LoginRefusal> {
    let tenancy_enabled = tenant_config.is_some_and(TenantConfig::is_enabled);
    if !tenancy_enabled {
        return Ok(());
    }
    if roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE) {
        return Ok(());
    }
    if memberships.is_empty() {
        return Err(LoginRefusal::NoTenantAssigned);
    }
    Ok(())
}

/// A login or refresh request that must be refused before token mint.
///
/// Separate from [`BuildLoginClaimsError`] so callers can distinguish a
/// policy refusal (user-actionable: contact admin to grant a membership)
/// from an internal failure (operator-actionable: misconfigured store).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginRefusal {
    NoTenantAssigned,
}

impl IntoResponse for LoginRefusal {
    fn into_response(self) -> Response {
        match self {
            Self::NoTenantAssigned => {
                let body = LoginErrorBody {
                    error: "no tenant assigned",
                    code: "UNAUTHORIZED",
                    status: 401,
                };
                (StatusCode::UNAUTHORIZED, Json(body)).into_response()
            }
        }
    }
}

/// Errors raised while building PASETO claims for login or refresh.
#[derive(Debug)]
pub(crate) enum BuildLoginClaimsError {
    /// The principal-claim config requires a User entity row, but the caller
    /// did not provide one. Indicates a wiring bug, not a user-facing fault.
    MissingUserEntity,
    /// A `required` source field resolved to null/missing — surfaces as 401.
    NullRequired(PrincipalClaimsError),
    /// Other principal-claim projection failure (e.g. type mismatch at runtime).
    PrincipalClaim(PrincipalClaimsError),
    /// Serializing the membership set into the PASETO custom-claim JSON
    /// failed. `TenantRef` derives Serialize so this is effectively unreachable
    /// in practice, but kept as a distinct variant so the handler returns a
    /// 500 (server error) rather than a 401 (user error).
    MembershipSerialize(serde_json::Error),
    /// `acton-service` failed to assemble the claim envelope.
    Acton(ActonError),
}

impl fmt::Display for BuildLoginClaimsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUserEntity => f.write_str(
                "principal-claim sources are configured but no user entity row was provided",
            ),
            Self::NullRequired(e) | Self::PrincipalClaim(e) => write!(f, "{e}"),
            Self::MembershipSerialize(e) => write!(f, "tenant_chain serialize failed: {e}"),
            Self::Acton(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BuildLoginClaimsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingUserEntity => None,
            Self::NullRequired(e) | Self::PrincipalClaim(e) => Some(e),
            Self::MembershipSerialize(e) => Some(e),
            Self::Acton(e) => Some(e),
        }
    }
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
        let mappings = PrincipalClaimMappings::default();
        let claims = build_login_claims(
            "alice",
            &["admin".to_string(), "hr".to_string()],
            None,
            &mappings,
            &[],
        )
        .unwrap();
        assert_eq!(claims.sub, "user:alice");
        assert_eq!(claims.roles, vec!["admin".to_string(), "hr".to_string()]);
        assert_eq!(claims.username.as_deref(), Some("alice"));
        assert_eq!(claims.iss.as_deref(), Some("schemaforge"));
    }

    #[test]
    fn build_login_claims_with_no_roles() {
        let mappings = PrincipalClaimMappings::default();
        let claims = build_login_claims("bob", &[], None, &mappings, &[]).unwrap();
        assert_eq!(claims.sub, "user:bob");
        assert!(claims.roles.is_empty());
    }

    #[test]
    fn build_login_claims_with_no_memberships_omits_tenant_chain() {
        let mappings = PrincipalClaimMappings::default();
        let claims = build_login_claims("alice", &[], None, &mappings, &[]).unwrap();
        // The custom map exists, but no `tenant_chain` key should be set.
        assert!(
            claims
                .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
                .is_none(),
            "expected no tenant_chain claim when memberships is empty"
        );
    }

    #[test]
    fn build_login_claims_projects_memberships_into_tenant_chain() {
        let mappings = PrincipalClaimMappings::default();
        let memberships = vec![
            TenantRef {
                schema: "Organization".to_string(),
                entity_id: "org-a".to_string(),
            },
            TenantRef {
                schema: "Organization".to_string(),
                entity_id: "org-b".to_string(),
            },
        ];
        let claims =
            build_login_claims("alice", &[], None, &mappings, &memberships).unwrap();
        let chain = claims
            .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
            .expect("tenant_chain populated");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].entity_id, "org-a");
        assert_eq!(chain[1].entity_id, "org-b");
    }

    #[test]
    fn enforce_policy_no_tenancy_always_allows() {
        let res = enforce_tenant_membership_policy(&[], &[], None);
        assert!(res.is_ok());
    }

    #[test]
    fn enforce_policy_platform_admin_bypasses_when_tenancy_enabled() {
        use schema_forge_core::types::{
            Annotation, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaId,
            SchemaName, TenantKind, TextConstraints,
        };
        use schema_forge_core::types::SchemaDefinition;
        // Minimal @tenant(root) schema so TenantConfig::is_enabled() is true.
        let root = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Organization").unwrap(),
            vec![FieldDefinition::with_annotations(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
                vec![],
            )],
            vec![Annotation::Tenant(TenantKind::Root)],
        )
        .unwrap();
        let cfg = TenantConfig::from_schemas(&[root]).unwrap();
        assert!(cfg.is_enabled());

        let res = enforce_tenant_membership_policy(
            &[],
            &[PLATFORM_ADMIN_ROLE.to_string()],
            Some(&cfg),
        );
        assert!(res.is_ok(), "platform_admin must bypass zero-membership refusal");
    }

    #[test]
    fn enforce_policy_zero_membership_with_enabled_tenancy_refuses() {
        use schema_forge_core::types::{
            Annotation, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaId,
            SchemaName, TenantKind, TextConstraints,
        };
        use schema_forge_core::types::SchemaDefinition;
        let root = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Organization").unwrap(),
            vec![FieldDefinition::with_annotations(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
                vec![],
            )],
            vec![Annotation::Tenant(TenantKind::Root)],
        )
        .unwrap();
        let cfg = TenantConfig::from_schemas(&[root]).unwrap();

        let res = enforce_tenant_membership_policy(
            &[],
            &["member".to_string()],
            Some(&cfg),
        );
        assert_eq!(res, Err(LoginRefusal::NoTenantAssigned));
    }

    #[test]
    fn enforce_policy_membership_present_allows() {
        let memberships = vec![TenantRef {
            schema: "Organization".to_string(),
            entity_id: "org-a".to_string(),
        }];
        let res = enforce_tenant_membership_policy(&memberships, &[], None);
        assert!(res.is_ok());
    }
}
