//! User invitation endpoints (issue #71).
//!
//! Invitation routes, all nested under `/api/v1/forge/`:
//!
//! - `POST /auth/invites` (authenticated) — an operator invites an address
//!   into the deployment, optionally scoping the invitee to a tenant and a
//!   role. Independent `InviteUser` permission and role-grant guards run at
//!   invite time. The minted invitation is persisted and an accept link is
//!   emailed or returned for manual sharing, depending on delivery mode.
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
//! Acceptance atomically claims the invitation before the first account or
//! membership write. Revocation and acceptance compete for the same pending
//! state, so an accepted claim cannot resurrect a revoked invitation. A later
//! provisioning failure leaves the claim consumed and requires operator repair.
//! `GET /auth/invites` lists secret-free pending projections, and
//! `DELETE /auth/invites/{id}` revokes one using independent Cedar actions.

use std::sync::Arc;
use std::time::Duration;

use crate::actor::ForgeActor;
use crate::messages::{GetTenantConfig, ReplyChannel};
use acton_service::audit::AuditSeverity;
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::middleware::paseto::PasetoAuth;
use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use schema_forge_backend::user_store::ForgeUser;
use schema_forge_backend::Entity;
use schema_forge_backend::{tenant::TenantConfig, TenantRef};
use schema_forge_backend::{
    ForgeInvitation, InvitationListQuery, InviteStatus, InviteStore, NewInvitation,
};
use schema_forge_core::types::{DynamicValue, EntityId};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::instrument;

use crate::access::OptionalClaims;
use crate::authz::engine::authorize;
use crate::authz::namespace::ActionVerb;
use crate::config::SchemaForgeConfig;
use crate::email::{EmailDelivery, EmailMessage, EmailSender};
use crate::error::ForgeError;
use crate::invite::{mint_invite_token, verify_invite_token, InviteTokenParams};
use crate::middleware::tenant_scope::{parse_active_tenant, ACTIVE_TENANT_HEADER};
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
    /// Transport used, explicitly distinguishing a link from an emailed invite.
    pub delivery: EmailDelivery,
    /// Shareable link to the persisted invitation.
    pub accept_url: String,
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

/// Pagination for `GET /auth/invites`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListInvitesRequest {
    /// Maximum pending candidates to inspect, from 1 to 100 (default 50).
    pub limit: Option<usize>,
    /// Storage continuation offset, at most 1,000,000 (default zero).
    pub offset: Option<usize>,
}

/// Secret-free projection of a pending invitation.
#[derive(Debug, Serialize)]
pub struct PendingInviteResponse {
    /// Persisted row identifier, used by the revoke endpoint.
    pub id: EntityId,
    /// Invited email address.
    pub email: String,
    /// Role offered by the invitation.
    pub role: Option<String>,
    /// Subject that issued the invitation, when known.
    pub inviter: Option<String>,
    /// UTC creation time from the UUIDv7 row identifier, or null for legacy ids.
    pub created_at: Option<DateTime<Utc>>,
    /// UTC expiry of the pending invitation.
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<ForgeInvitation> for PendingInviteResponse {
    fn from(invite: ForgeInvitation) -> Self {
        Self {
            created_at: invite.id.created_at(),
            id: invite.id,
            email: invite.email,
            role: invite.role,
            inviter: invite.invited_by,
            expires_at: invite.expires_at,
        }
    }
}

/// Bounded pending-invitation page. No acceptance credentials are returned.
#[derive(Debug, Serialize)]
pub struct ListInvitesResponse {
    /// Invitations visible under both active scope and Cedar policy.
    pub invitations: Vec<PendingInviteResponse>,
    /// Continue with this raw storage offset, or null when exhausted.
    pub next_offset: Option<usize>,
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
        Some(b) => format!(
            "{}/invite/accept?invite={invite_id}",
            b.trim_end_matches('/')
        ),
        None => format!("/invite/accept?invite={invite_id}"),
    }
}

/// Plain-text invitation email body, branded with the deployment's
/// `project_name` so onboarding users see the application they are joining
/// (e.g. "Bob's Dog Scheduling") rather than the underlying engine.
fn invite_email_body(
    project_name: &str,
    accept_url: &str,
    password_login: bool,
    providers: &[String],
) -> String {
    let providers = providers
        .iter()
        .map(|provider| match provider.as_str() {
            "github" => "GitHub",
            "google" => "Google",
            "microsoft" => "Microsoft",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" or ");
    let instructions = match (providers.is_empty(), password_login) {
        (false, false) => format!("To accept the invitation, sign in with {providers} using this email address. Open:"),
        (false, true) => format!("To accept the invitation, sign in with {providers} using this email address, or set your password. Open:"),
        (true, true) => "To accept the invitation and set your password, open:".into(),
        (true, false) => "To accept the invitation, open:".into(),
    };
    format!(
        "You have been invited to join {project_name}.\n\n\
         {instructions}\n\n  {accept_url}\n\n\
         If you were not expecting this invitation you can ignore this message.\n"
    )
}

/// Subject line for the invitation email, branded with `project_name`.
fn invite_email_subject(project_name: &str) -> String {
    format!("You've been invited to {project_name}")
}

/// Validate the signed invitation's tenant target before minting or persistence.
fn validate_invite_tenant(
    claims: &Claims,
    config: Option<&TenantConfig>,
    tenant_type: Option<&str>,
    tenant_id: Option<&str>,
) -> Result<(), ForgeError> {
    let (tenant_type, tenant_id) = match (tenant_type, tenant_id) {
        (None, None) => return Ok(()),
        (Some(kind), Some(id)) if !kind.is_empty() && !id.is_empty() => (kind, id),
        _ => {
            return Err(ForgeError::ValidationFailed {
                details: vec![
                    "tenant_type and tenant_id must be supplied together and must not be empty"
                        .into(),
                ],
            })
        }
    };
    if !config.is_some_and(|config| {
        config
            .hierarchy
            .iter()
            .any(|level| level.schema.as_str() == tenant_type)
    }) {
        return Err(ForgeError::ValidationFailed {
            details: vec!["tenant_type must name a configured tenant schema".into()],
        });
    }
    let chain = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();
    if claims.has_role("platform_admin")
        || chain
            .iter()
            .any(|tenant| tenant.schema == tenant_type && tenant.entity_id == tenant_id)
    {
        Ok(())
    } else {
        Err(ForgeError::Forbidden {
            message: "invitation tenant is outside the caller's effective tenant scope".into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn unavailable_invitation() -> ForgeError {
    ForgeError::ValidationFailed {
        details: vec!["invitation is expired or already used".into()],
    }
}

fn invitation_not_found(id: &EntityId) -> ForgeError {
    ForgeError::EntityNotFound {
        schema: "ForgeInvitation".into(),
        entity_id: id.to_string(),
    }
}

async fn invitation_scope(
    state: &AppState<SchemaForgeConfig>,
    claims: &Claims,
    headers: &HeaderMap,
) -> Result<Option<TenantRef>, ForgeError> {
    let requested = headers
        .get(ACTIVE_TENANT_HEADER)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(parse_active_tenant)
                .ok_or_else(|| ForgeError::InvalidQuery {
                    message: "X-Active-Tenant must be <schema>:<entity_id>".into(),
                })
        })
        .transpose()?;
    if claims.has_role("platform_admin") {
        let Some((schema, entity_id)) = requested else {
            return Ok(None);
        };
        if EntityId::parse(&entity_id).is_err() {
            return Err(ForgeError::InvalidQuery {
                message: "X-Active-Tenant must contain a valid entity id".into(),
            });
        }
        let forge = state
            .actor::<ForgeActor>()
            .ok_or_else(|| ForgeError::Internal {
                message: "ForgeActor not registered".into(),
            })?;
        let (tx, rx) = oneshot::channel();
        forge
            .send(GetTenantConfig {
                reply: ReplyChannel::new(tx),
            })
            .await;
        let config = rx.await.map_err(|_| ForgeError::Internal {
            message: "ForgeActor tenant configuration reply channel dropped".into(),
        })?;
        if !config.is_some_and(|config| {
            config
                .hierarchy
                .iter()
                .any(|level| level.schema.as_str() == schema)
        }) {
            return Err(ForgeError::InvalidQuery {
                message: "X-Active-Tenant must name a configured tenant schema".into(),
            });
        }
        return Ok(Some(TenantRef { schema, entity_id }));
    }
    // The middleware rewrites tenant_chain to the validated root-to-active-leaf
    // walk. Membership in an ancestor never widens this invitation operation.
    let chain = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();
    let active = chain.last().ok_or_else(|| ForgeError::Forbidden {
        message: "an active tenant is required to manage invitations".into(),
    })?;
    if requested.is_some_and(|(schema, id)| schema != active.schema || id != active.entity_id) {
        return Err(ForgeError::Forbidden {
            message: "invitation tenant is outside the caller's active tenant scope".into(),
        });
    }
    Ok(Some(active.clone()))
}

fn invitation_in_scope(invite: &ForgeInvitation, scope: Option<&TenantRef>) -> bool {
    scope.is_none_or(|tenant| {
        invite.tenant_type.as_deref() == Some(tenant.schema.as_str())
            && invite.tenant_id.as_deref() == Some(tenant.entity_id.as_str())
    })
}

fn invitation_resource(invite: &ForgeInvitation, policies: &crate::authz::PolicyStore) -> Entity {
    let user = ForgeUser {
        username: invite.email.clone(),
        roles: invite.role.iter().cloned().collect(),
        display_name: invite.display_name.clone(),
        active: true,
        role_rank: 0,
    };
    let mut resource = forge_user_to_user_entity(&user, policies);
    // Let concrete policies identify the pending invitation by its safe row id.
    resource.id = invite.id.clone();
    if let Some(tenant_id) = &invite.tenant_id {
        resource
            .fields
            .insert("_tenant".into(), DynamicValue::Text(tenant_id.clone()));
    }
    resource
}

/// `GET /auth/invites`: list the active tenant's pending invitations.
///
/// `ListInvites` must permit the tenant's schema-level listing resource and each
/// concrete proposed User. Custom per-row denies omit that invitation.
/// Platform administrators may list all scopes. These actions do not grant
/// roles, so the invitation creation rank guard does not apply.
#[instrument(skip_all)]
pub async fn list_invites(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(invite_store): Extension<Arc<dyn InviteStore>>,
    OptionalClaims(claims): OptionalClaims,
    headers: HeaderMap,
    Query(query): Query<ListInvitesRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);
    if !(1..=100).contains(&limit) || offset > 1_000_000 {
        return Err(ForgeError::InvalidQuery {
            message: "limit must be 1..100 and offset must be at most 1000000".into(),
        });
    }
    let scope = invitation_scope(&state, claims, &headers).await?;
    let schema = fetch_user_schema(&state).await?;
    let policies = fetch_policy_store(&state).await?;
    let mut listing = forge_user_to_user_entity(
        &ForgeUser {
            username: String::new(),
            roles: Vec::new(),
            display_name: None,
            active: true,
            role_rank: 0,
        },
        &policies,
    );
    if let Some(tenant) = &scope {
        listing.fields.insert(
            "_tenant".into(),
            DynamicValue::Text(tenant.entity_id.clone()),
        );
    }
    let prepared = crate::authz::engine::PreparedAuthorization::new(
        policies.current(),
        Some(claims),
        ActionVerb::ListInvites,
        &schema,
    )
    .map_err(|_| ForgeError::Internal {
        message: "invitation authorization failed".into(),
    })?;
    if !prepared
        .authorize(Some(&listing))
        .map_err(|_| ForgeError::Internal {
            message: "invitation authorization failed".into(),
        })?
        .is_allow()
    {
        return Err(ForgeError::Forbidden {
            message: "access denied for ListInvites".into(),
        });
    }
    let page = invite_store
        .list_pending(&InvitationListQuery {
            tenant: scope.clone(),
            limit,
            offset,
            at: Utc::now(),
        })
        .await?;
    let mut invitations = Vec::with_capacity(page.invitations.len());
    for invite in page.invitations {
        // Also protect against a custom store returning out-of-scope rows.
        if !invitation_in_scope(&invite, scope.as_ref()) || !invite.is_acceptable(Utc::now()) {
            continue;
        }
        let resource = invitation_resource(&invite, &policies);
        if prepared
            .authorize(Some(&resource))
            .map_err(|_| ForgeError::Internal {
                message: "invitation authorization failed".into(),
            })?
            .is_allow()
        {
            invitations.push(invite.into());
        }
    }
    Ok(Json(ListInvitesResponse {
        invitations,
        next_offset: page.next_offset,
    }))
}

/// `DELETE /auth/invites/{id}`: atomically revoke a visible invitation.
///
/// Already revoked invitations return 204; a consumed invitation returns 409.
/// Foreign-tenant rows and absent rows share the same 404 response.
#[instrument(skip_all)]
pub async fn revoke_invite(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(invite_store): Extension<Arc<dyn InviteStore>>,
    OptionalClaims(claims): OptionalClaims,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let scope = invitation_scope(&state, claims, &headers).await?;
    let id = EntityId::parse(&id).map_err(|_| ForgeError::InvalidQuery {
        message: "invitation id must be a valid row identifier".into(),
    })?;
    let invite = invite_store
        .find_by_id(&id)
        .await?
        .filter(|invite| invitation_in_scope(invite, scope.as_ref()))
        .ok_or_else(|| invitation_not_found(&id))?;
    let schema = fetch_user_schema(&state).await?;
    let policies = fetch_policy_store(&state).await?;
    let resource = invitation_resource(&invite, &policies);
    if !authorize(
        &policies,
        Some(claims),
        ActionVerb::RevokeInvite,
        &schema,
        Some(&resource),
    )
    .map_err(|_| ForgeError::Internal {
        message: "invitation authorization failed".into(),
    })?
    .is_allow()
    {
        return Err(ForgeError::Forbidden {
            message: "access denied for RevokeInvite".into(),
        });
    }
    if invite.status == InviteStatus::Revoked {
        return Ok(StatusCode::NO_CONTENT);
    }
    if !invite_store.revoke(&id, Utc::now()).await? {
        return Err(ForgeError::Conflict {
            reason: "invitation_not_pending",
            message: "invitation is no longer pending".into(),
        });
    }
    audit_user(
        &state,
        "forge.invite.revoked",
        AuditSeverity::Notice,
        &claims.sub,
        id.as_str(),
        Some(serde_json::json!({
            "entity_id": id, "tenant_id": invite.tenant_id,
        })),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /auth/invites` — issue an invitation.
///
/// Authorization evaluates `InviteUser` against a proposed `User` carrying
/// the granted role rank and target `_tenant`. The concrete check supports
/// target-specific policies without a placeholder preflight denying them.
/// `CreateUser` never grants invitation permission. The explicit platform
/// administrator grant restriction and Cedar role-rank forbid still apply.
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

    validate_email(&body.email)?;
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetTenantConfig {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let tenant_config = rx.await.map_err(|_| ForgeError::Internal {
        message: "ForgeActor reply channel dropped while fetching tenant configuration".into(),
    })?;
    validate_invite_tenant(
        claims,
        tenant_config.as_ref(),
        body.tenant_type.as_deref(),
        body.tenant_id.as_deref(),
    )?;

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
    let mut proposed_entity = forge_user_to_user_entity(&proposed, policy_store.as_ref());
    if let Some(tenant_id) = &body.tenant_id {
        proposed_entity.fields.insert(
            "_tenant".into(),
            schema_forge_core::types::DynamicValue::Text(tenant_id.clone()),
        );
    }
    let decision = authorize(
        &policy_store,
        Some(claims),
        ActionVerb::Invite,
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
                "reason": "invite_policy_denied",
            })),
        )
        .await;
        return Err(ForgeError::Forbidden {
            message: format!("access denied for InviteUser with roles {proposed_roles:?}"),
        });
    }

    // Refuse to invite an address that already has an account.
    if auth_store.get_user(&body.email).await?.is_some() {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!("user '{}' already exists", body.email)],
        });
    }

    let email_config = &state.config().custom.schema_forge.email;
    email_config
        .validate_link_delivery()
        .map_err(|error| ForgeError::Internal {
            message: error.to_string(),
        })?;
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

    // Persisted links can always be shared. Link mode skips the transport;
    // an SMTP failure returns the same reference with an explicit failed
    // delivery outcome, leaving the invitation Pending.
    let accept_url = build_accept_url(
        email_config
            .public_base_url
            .as_deref()
            .or_else(|| email_sender.public_base_url()),
        &invitation.jti,
    );
    let project_name = &state.config().custom.schema_forge.project_name;
    let oauth = &state.config().custom.schema_forge.auth.oauth;
    #[cfg(feature = "oauth")]
    let providers = state
        .config()
        .auth
        .as_ref()
        .and_then(|auth| auth.oauth.as_ref())
        .filter(|config| oauth.enabled && config.enabled)
        .map(|config| config.providers.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    #[cfg(not(feature = "oauth"))]
    let providers = Vec::new();
    let message = EmailMessage {
        to: body.email.clone(),
        subject: invite_email_subject(project_name),
        body_text: invite_email_body(
            project_name,
            &accept_url,
            !oauth.enabled || oauth.password_login,
            &providers,
        ),
    };
    if email_config.delivery == EmailDelivery::Smtp {
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
            return Err(ForgeError::InviteDeliveryFailed {
                invite_id: invitation.jti,
                accept_url,
            });
        }
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
            "delivery": email_config.delivery,
        })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            invite_id: invitation.jti,
            email: body.email,
            expires_at: minted.expires_at.to_rfc3339(),
            delivery: email_config.delivery,
            accept_url,
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
        return Err(unavailable_invitation());
    }

    // Reconstruct + verify the full token from the stored column. Signed
    // claims are authoritative; the DB columns are a convenience mirror.
    let verified = verify_invite_token(validator.as_ref(), &invite.token).map_err(|_| {
        ForgeError::Internal {
            message: "stored invitation failed verification".into(),
        }
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

    // Claim pending state before any provisioning write. A successful revoke
    // and a successful acceptance cannot both win this atomic transition.
    if !invite_store.try_consume(&invite.id, Utc::now()).await? {
        return Err(unavailable_invitation());
    }
    auth_store
        .create_user(&verified.email, &body.password, &roles, &display_name)
        .await?;

    if let (Some(tt), Some(tid)) = (
        verified.tenant_type.as_deref(),
        verified.tenant_id.as_deref(),
    ) {
        auth_store
            .add_tenant_membership(&verified.email, tt, tid, verified.role.as_deref())
            .await?;
    }

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
    fn invitation_target_requires_configured_type_and_effective_membership() {
        use schema_forge_backend::tenant::TenantLevel;
        use schema_forge_core::types::{EntityId, SchemaName};
        let mut claims: Claims = serde_json::from_value(serde_json::json!({
            "sub": "user_inviter", "roles": ["owner"], "perms": [], "exp": 9999999999_u64,
            "tenant_chain": [{"schema": "Org", "entity_id": "org_a"}]
        }))
        .unwrap();
        // Claims custom fields are explicitly populated to match middleware output.
        claims.custom.insert(
            "tenant_chain".into(),
            serde_json::json!([{"schema": "Org", "entity_id": "org_a"}]),
        );
        let config = TenantConfig {
            root_schema: Some(SchemaName::new("Org").unwrap()),
            hierarchy: vec![TenantLevel {
                schema: SchemaName::new("Org").unwrap(),
                parent: None,
                parent_field: None,
            }],
        };
        assert!(validate_invite_tenant(&claims, Some(&config), Some("Org"), Some("org_a")).is_ok());
        assert!(matches!(
            validate_invite_tenant(
                &claims,
                Some(&config),
                Some("Org"),
                Some(EntityId::new("org").as_str())
            ),
            Err(ForgeError::Forbidden { .. })
        ));
        assert!(matches!(
            validate_invite_tenant(&claims, Some(&config), Some("Other"), Some("org_a")),
            Err(ForgeError::ValidationFailed { .. })
        ));
        assert!(validate_invite_tenant(&claims, Some(&config), Some("Org"), None).is_err());
        assert!(validate_invite_tenant(&claims, Some(&config), None, None).is_ok());
        claims.roles = vec!["platform_admin".into()];
        assert!(validate_invite_tenant(&claims, Some(&config), Some("Org"), Some("org_b")).is_ok());
        assert!(
            validate_invite_tenant(&claims, Some(&config), Some("Unknown"), Some("org_b")).is_err()
        );
    }

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
            true,
            &[],
        );
        assert!(body.contains("https://app.agency.gov/invite/accept?invite=xyz"));
    }

    #[test]
    fn invitation_wording_matches_available_login_modes() {
        let providers = ["github".into(), "google".into()];
        let oauth_only = invite_email_body("App", "https://app/invite", false, &providers);
        assert!(oauth_only.contains("sign in with GitHub or Google using this email address"));
        assert!(!oauth_only.contains("password"));
        let mixed = invite_email_body("App", "https://app/invite", true, &providers);
        assert!(mixed.contains("sign in with GitHub or Google"));
        assert!(mixed.contains("or set your password"));
        let password_only = invite_email_body("App", "https://app/invite", true, &[]);
        assert!(password_only.contains("set your password"));
        assert!(!password_only.contains("sign in with"));
    }

    #[test]
    fn invite_email_is_branded_with_project_name() {
        let body = invite_email_body(
            "Bob's Dog Scheduling",
            "https://x/invite?invite=1",
            true,
            &[],
        );
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
