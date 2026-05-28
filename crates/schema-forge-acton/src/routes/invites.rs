//! User invitation endpoints (issue #71).
//!
//! Two routes, both nested under `/api/v1/forge/`:
//!
//! - `POST /auth/invites` (authenticated) — an operator invites an address
//!   into the deployment, optionally scoping the invitee to a tenant and a
//!   role. The same privilege guards that gate `POST /users` run here, *at
//!   invite time*, so an invite can never grant access the inviter could not
//!   grant directly. The minted invitation is persisted and an accept link is
//!   emailed to the invitee.
//!
//! - `POST /auth/invites/accept` (public) — the invitee presents the opaque
//!   `invite_id` from their email plus a chosen password. The server
//!   reconstructs the full PASETO from the stored row, re-verifies it (the
//!   *signed* claims are authoritative over the database columns), creates the
//!   `User` account and any `TenantMembership` in the same request, then marks
//!   the invitation consumed so the link cannot be replayed.
//!
//! # Why create the account at accept, not at invite
//!
//! Creating the `User` only when the invitee accepts avoids leaving a
//! half-provisioned, password-less account sitting in the table between invite
//! and acceptance. The invitation row *is* the pending state. The privilege
//! checks still happen at invite time against the proposed role, so deferring
//! account creation does not weaken authorization.
//!
//! The account creation and the membership write are two store calls with no
//! spanning transaction primitive available. We order them so the invitation
//! is consumed **last**: a failure between the two writes leaves the invite
//! `Pending` (retryable) rather than burning a link against a partially
//! created account.

use std::sync::Arc;
use std::time::Duration;

use acton_service::audit::AuditSeverity;
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::middleware::paseto::PasetoAuth;
use acton_service::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use chrono::Utc;
use schema_forge_backend::user_store::ForgeUser;
use schema_forge_backend::{InviteStore, NewInvitation};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::access::{check_schema_access, AccessAction, OptionalClaims};
use crate::authz::engine::authorize;
use crate::authz::namespace::ActionVerb;
use crate::config::SchemaForgeConfig;
use crate::email::{EmailMessage, EmailSender};
use crate::error::ForgeError;
use crate::invite::{mint_invite_token, verify_invite_token, InviteTokenParams};
use crate::routes::users::{
    audit_user, caller_can_grant_roles, fetch_policy_store, fetch_user_schema,
    forge_user_to_user_entity, require_auth, validate_password,
};
use crate::state::DynAuthStore;

/// How long a minted invitation stays valid (7 days).
///
/// Long enough to survive a weekend and a missed inbox, short enough that a
/// leaked link expires on a human timescale. Sets both the PASETO `exp` and
/// the stored `expires_at`.
pub const INVITE_TOKEN_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const MAX_EMAIL_LEN: usize = 512;

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// Request body for `POST /auth/invites`.
#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    /// Invitee email. Becomes the future `User.email` (the login identifier).
    pub email: String,
    /// Optional display name to seed onto the future account.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Tenant root type the invitee is being added to (e.g. `"Organization"`).
    #[serde(default)]
    pub tenant_type: Option<String>,
    /// Tenant root entity id.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Role granted to the invitee — used as both their `User` role and their
    /// `TenantMembership` role for this slice.
    #[serde(default)]
    pub role: Option<String>,
}

/// Success body for `POST /auth/invites`.
#[derive(Debug, Serialize)]
pub struct CreateInviteResponse {
    /// The opaque invitation reference emailed to the invitee.
    pub invite_id: String,
    /// The invited email.
    pub email: String,
    /// ISO-8601 UTC expiry.
    pub expires_at: String,
}

/// Request body for `POST /auth/invites/accept`.
#[derive(Debug, Deserialize)]
pub struct AcceptInviteRequest {
    /// The opaque invitation reference from the emailed link.
    pub invite_id: String,
    /// The password the invitee chooses for their new account.
    pub password: String,
    /// Optional display name override; falls back to the invite's, then email.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Success body for `POST /auth/invites/accept`.
#[derive(Debug, Serialize)]
pub struct AcceptInviteResponse {
    /// The created account's login identifier (its email).
    pub email: String,
    /// Roles granted to the new account.
    pub roles: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Minimal structural email validation. Not RFC 5322 — just enough to reject
/// obvious garbage before minting a token and writing a row. The real proof an
/// address is deliverable is the invitee receiving and clicking the link.
fn validate_email(email: &str) -> Result<(), ForgeError> {
    let invalid = |msg: &str| {
        Err(ForgeError::ValidationFailed {
            details: vec![msg.to_string()],
        })
    };
    if email.is_empty() {
        return invalid("email must not be empty");
    }
    if email.len() > MAX_EMAIL_LEN {
        return invalid("email exceeds maximum length of 512");
    }
    if email.chars().any(char::is_whitespace) {
        return invalid("email must not contain whitespace");
    }
    let Some((local, domain)) = email.split_once('@') else {
        return invalid("email must contain exactly one '@'");
    };
    if local.is_empty() || domain.is_empty() {
        return invalid("email must have text on both sides of '@'");
    }
    if domain.contains('@') {
        return invalid("email must contain exactly one '@'");
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return invalid("email domain must contain a dot");
    }
    Ok(())
}

/// Build the absolute accept link the invitee clicks. Falls back to a
/// site-relative path when no public base URL is configured.
fn build_accept_url(base: Option<&str>, invite_id: &str) -> String {
    match base {
        Some(b) => format!("{}/invite/accept?invite={invite_id}", b.trim_end_matches('/')),
        None => format!("/invite/accept?invite={invite_id}"),
    }
}

/// Plain-text invitation email body, branded with the deployment's
/// `project_name` so onboarding users see the application they are joining
/// (e.g. "Bob's Dog Scheduling") rather than the underlying engine.
fn invite_email_body(project_name: &str, accept_url: &str) -> String {
    format!(
        "You have been invited to join {project_name}.\n\n\
         To accept the invitation and set your password, open:\n\n  {accept_url}\n\n\
         If you were not expecting this invitation you can ignore this message.\n"
    )
}

/// Subject line for the invitation email, branded with `project_name`.
fn invite_email_subject(project_name: &str) -> String {
    format!("You've been invited to {project_name}")
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /auth/invites` — issue an invitation.
///
/// Authorization mirrors `POST /users` exactly: schema-level `CreateUser`
/// access via [`check_schema_access`], the `platform_admin`-grant guard via
/// [`caller_can_grant_roles`], and the Cedar `role_rank` guard against a
/// synthetic `User` carrying the proposed role. Running these here means an
/// invitation can never confer access the inviter lacks the authority to
/// grant directly.
#[instrument(skip_all)]
pub async fn create_invite(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Extension(generator): Extension<Arc<PasetoGenerator>>,
    Extension(email_sender): Extension<Arc<dyn EmailSender>>,
    Extension(invite_store): Extension<Arc<dyn InviteStore>>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<CreateInviteRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;

    check_schema_access(&policy_store, &user_schema, Some(claims), AccessAction::Create)?;

    validate_email(&body.email)?;

    // The invite grants a single role (or none). Apply the same role-grant
    // guards `create_user` applies to its `roles` list.
    let proposed_roles: Vec<String> = body
        .role
        .iter()
        .filter(|r| !r.is_empty())
        .cloned()
        .collect();
    caller_can_grant_roles(claims, &proposed_roles)?;

    let proposed = ForgeUser {
        username: body.email.clone(),
        roles: proposed_roles.clone(),
        display_name: body.display_name.clone(),
        active: true,
        role_rank: 0,
    };
    let proposed_entity = forge_user_to_user_entity(&proposed, policy_store.as_ref());
    let decision = authorize(
        &policy_store,
        Some(claims),
        ActionVerb::Create,
        &user_schema,
        Some(&proposed_entity),
    )
    .map_err(|e| ForgeError::Internal {
        message: format!("authz engine error during create_invite: {e}"),
    })?;
    if !decision.is_allow() {
        audit_user(
            &state,
            "forge.access.denied",
            AuditSeverity::Warning,
            &claims.sub,
            &body.email,
            Some(serde_json::json!({
                "action": "create_invite",
                "proposed_roles": proposed_roles,
                "reason": "role_rank_guard",
            })),
        )
        .await;
        return Err(ForgeError::Forbidden {
            message: format!(
                "inviting a user with roles {proposed_roles:?} would exceed caller's role_rank"
            ),
        });
    }

    // Refuse to invite an address that already has an account.
    if auth_store.get_user(&body.email).await?.is_some() {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!("user '{}' already exists", body.email)],
        });
    }

    let minted = mint_invite_token(
        &generator,
        &InviteTokenParams {
            email: body.email.clone(),
            tenant_type: body.tenant_type.clone(),
            tenant_id: body.tenant_id.clone(),
            role: body.role.clone(),
        },
        INVITE_TOKEN_LIFETIME,
    )
    .map_err(|e| ForgeError::Internal {
        message: format!("failed to mint invite token: {e}"),
    })?;

    // Persist before emailing so the accept link always resolves to a row.
    let invitation = invite_store
        .create(NewInvitation {
            email: body.email.clone(),
            display_name: body.display_name.clone(),
            tenant_type: body.tenant_type.clone(),
            tenant_id: body.tenant_id.clone(),
            role: body.role.clone(),
            jti: minted.invite_id.clone(),
            token: minted.token.clone(),
            expires_at: minted.expires_at,
            invited_by: Some(claims.sub.clone()),
        })
        .await?;

    // Deliver the accept link. Fail closed: a delivery failure surfaces as a
    // 5xx so the operator knows the invite did not reach the invitee. The row
    // stays `Pending`, so the invite can be re-sent once SMTP is healthy.
    let accept_url = build_accept_url(email_sender.public_base_url(), &invitation.jti);
    let project_name = &state.config().custom.schema_forge.project_name;
    let message = EmailMessage {
        to: body.email.clone(),
        subject: invite_email_subject(project_name),
        body_text: invite_email_body(project_name, &accept_url),
    };
    if let Err(e) = email_sender.send(message).await {
        audit_user(
            &state,
            "forge.invite.send_failed",
            AuditSeverity::Error,
            &claims.sub,
            &body.email,
            Some(serde_json::json!({
                "invite_id": invitation.jti,
                "error": e.to_string(),
            })),
        )
        .await;
        return Err(ForgeError::Internal {
            message: format!("invitation stored but email delivery failed: {e}"),
        });
    }

    audit_user(
        &state,
        "forge.invite.created",
        AuditSeverity::Notice,
        &claims.sub,
        &body.email,
        Some(serde_json::json!({
            "invite_id": invitation.jti,
            "tenant_type": body.tenant_type,
            "tenant_id": body.tenant_id,
            "role": body.role,
        })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            invite_id: invitation.jti,
            email: body.email,
            expires_at: minted.expires_at.to_rfc3339(),
        }),
    ))
}

/// `POST /auth/invites/accept` — accept an invitation and onboard the user.
///
/// Public (the invitee has no account yet); gated entirely by possession of a
/// valid, unexpired, unconsumed invitation. The full token never leaves the
/// server — the client sends only the opaque `invite_id`, and the server
/// reconstructs the PASETO from the stored row and re-verifies it. Role and
/// tenant are read from the **signed** claims, defending against tampering
/// with the stored columns.
#[instrument(skip_all)]
pub async fn accept_invite(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Extension(invite_store): Extension<Arc<dyn InviteStore>>,
    Extension(validator): Extension<Arc<PasetoAuth>>,
    Json(body): Json<AcceptInviteRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    validate_password(&body.password)?;

    let invite = invite_store
        .find_by_jti(&body.invite_id)
        .await?
        .ok_or_else(|| ForgeError::ValidationFailed {
            details: vec!["invitation not found".to_string()],
        })?;

    let now = Utc::now();
    if !invite.is_acceptable(now) {
        let reason = if invite.is_expired(now) {
            "expired"
        } else {
            "not_pending"
        };
        audit_user(
            &state,
            "forge.invite.rejected",
            AuditSeverity::Warning,
            "anonymous",
            &invite.email,
            Some(serde_json::json!({ "invite_id": body.invite_id, "reason": reason })),
        )
        .await;
        return Err(ForgeError::ValidationFailed {
            details: vec!["invitation is expired or already used".to_string()],
        });
    }

    // Reconstruct + verify the full token from the stored column. Signed
    // claims are authoritative; the DB columns are a convenience mirror.
    let verified =
        verify_invite_token(validator.as_ref(), &invite.token).map_err(|e| ForgeError::Internal {
            message: format!("stored invite token failed verification: {e}"),
        })?;
    if verified.invite_id != invite.jti {
        return Err(ForgeError::Internal {
            message: "invite token does not match the invitation it was stored under".to_string(),
        });
    }

    if auth_store.get_user(&verified.email).await?.is_some() {
        return Err(ForgeError::Conflict {
            reason: "user_exists",
            message: format!("user '{}' already exists", verified.email),
        });
    }

    let roles: Vec<String> = verified
        .role
        .iter()
        .filter(|r| !r.is_empty())
        .cloned()
        .collect();
    let display_name = body
        .display_name
        .clone()
        .or_else(|| invite.display_name.clone())
        .unwrap_or_else(|| verified.email.clone());

    // Create the account, then grant the membership, then consume the invite
    // (consumed last — see the module docs on the non-atomic write ordering).
    auth_store
        .create_user(&verified.email, &body.password, &roles, &display_name)
        .await?;

    if let (Some(tt), Some(tid)) = (verified.tenant_type.as_deref(), verified.tenant_id.as_deref())
    {
        auth_store
            .add_tenant_membership(&verified.email, tt, tid, verified.role.as_deref())
            .await?;
    }

    invite_store.mark_consumed(&invite.id, now).await?;

    audit_user(
        &state,
        "forge.invite.accepted",
        AuditSeverity::Notice,
        &verified.email,
        &verified.email,
        Some(serde_json::json!({
            "invite_id": invite.jti,
            "roles": roles,
            "tenant_type": verified.tenant_type,
            "tenant_id": verified.tenant_id,
        })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(AcceptInviteResponse {
            email: verified.email,
            roles,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Unit tests for pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_email_accepts_plausible_addresses() {
        assert!(validate_email("invitee@example.gov").is_ok());
        assert!(validate_email("a.b-c+tag@sub.agency.gov").is_ok());
    }

    #[test]
    fn validate_email_rejects_garbage() {
        assert!(validate_email("").is_err());
        assert!(validate_email("no-at-sign").is_err());
        assert!(validate_email("two@@at.gov").is_err());
        assert!(validate_email("nodot@localhost").is_err());
        assert!(validate_email("has space@x.gov").is_err());
        assert!(validate_email("@x.gov").is_err());
        assert!(validate_email("user@").is_err());
        assert!(validate_email("user@x.").is_err());
    }

    #[test]
    fn validate_email_rejects_overlong() {
        let long = format!("{}@x.gov", "a".repeat(MAX_EMAIL_LEN));
        assert!(validate_email(&long).is_err());
    }

    #[test]
    fn build_accept_url_uses_base_and_trims_trailing_slash() {
        assert_eq!(
            build_accept_url(Some("https://app.agency.gov/"), "abc123"),
            "https://app.agency.gov/invite/accept?invite=abc123"
        );
        assert_eq!(
            build_accept_url(Some("https://app.agency.gov"), "abc123"),
            "https://app.agency.gov/invite/accept?invite=abc123"
        );
    }

    #[test]
    fn build_accept_url_falls_back_to_relative_path() {
        assert_eq!(
            build_accept_url(None, "abc123"),
            "/invite/accept?invite=abc123"
        );
    }

    #[test]
    fn invite_email_body_contains_link() {
        let body = invite_email_body(
            "SchemaForge",
            "https://app.agency.gov/invite/accept?invite=xyz",
        );
        assert!(body.contains("https://app.agency.gov/invite/accept?invite=xyz"));
    }

    #[test]
    fn invite_email_is_branded_with_project_name() {
        let body = invite_email_body("Bob's Dog Scheduling", "https://x/invite?invite=1");
        assert!(
            body.contains("join Bob's Dog Scheduling"),
            "email body must name the deployment, not the engine: {body}"
        );
        assert!(
            !body.contains("SchemaForge"),
            "branded body must not leak the engine name: {body}"
        );
        assert_eq!(
            invite_email_subject("Bob's Dog Scheduling"),
            "You've been invited to Bob's Dog Scheduling"
        );
    }
}
