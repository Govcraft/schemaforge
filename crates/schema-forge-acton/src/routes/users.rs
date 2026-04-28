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
//!   `platform_admin` role. Non-platform-admin callers cannot enumerate
//!   `platform_admin` users in the list response, cannot grant the
//!   `platform_admin` role to anyone, and cannot delete the last
//!   `platform_admin` (returned as 409 with reason `last_platform_admin`).
//! - `POST /users/:username/password` allows `platform_admin` OR self
//!   (the token's `sub` matches `user:<username>` or the bare username).
//!
//! Duplicate usernames are rejected up front via `AuthStore::get_user`
//! since neither backend surfaces a typed conflict error (both just
//! propagate the DB unique-constraint error as `QueryError`). This keeps
//! `ForgeError` untouched — the caller sees a `validation_failed`
//! envelope with a clear message.

use std::collections::BTreeMap;
use std::sync::Arc;

use acton_service::middleware::Claims;
use acton_service::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use schema_forge_backend::entity::Entity;
use schema_forge_backend::user_store::ForgeUser;
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition, SchemaName};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::instrument;

use crate::access::{
    check_schema_access, AccessAction, OptionalClaims, PLATFORM_ADMIN_ROLE,
};
use crate::actor::ForgeActor;
use crate::authz::engine::authorize;
use crate::authz::namespace::ActionVerb;
use crate::authz::PolicyStore;
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;
use crate::messages::{GetPolicyStore, GetSchema, ReplyChannel};
use crate::state::DynAuthStore;
use acton_service::prelude::ActorHandleInterface;

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


/// Fetch the User schema definition from the registry.
async fn fetch_user_schema(
    state: &AppState<SchemaForgeConfig>,
) -> Result<SchemaDefinition, ForgeError> {
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: "User".to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let result = rx.await.map_err(|_| ForgeError::Internal {
        message: "ForgeActor reply channel dropped while fetching User schema".into(),
    })?;
    result.ok_or_else(|| ForgeError::Internal {
        message: "User system schema is missing from the registry".into(),
    })
}

/// Fetch the current Cedar policy store from the actor.
async fn fetch_policy_store(
    state: &AppState<SchemaForgeConfig>,
) -> Result<Arc<PolicyStore>, ForgeError> {
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetPolicyStore {
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await
        .map_err(|_| ForgeError::Internal {
            message: "ForgeActor reply channel dropped while fetching PolicyStore".into(),
        })?
        .ok_or_else(|| ForgeError::Internal {
            message: "Cedar policy store not initialized — InitForge has not run".into(),
        })
}

/// Build a synthetic SchemaForge `Entity` of schema "User" for Cedar
/// authorization.
///
/// `_forge_users` and the User schema's entity table are still separate
/// data sources today (T19 collapses that duality). The Cedar engine
/// reasons about User entities, so we adapt every `ForgeUser` row into a
/// User entity just-in-time, computing `role_rank` from the role-rank
/// table the policy store already carries. This makes the global
/// `user_role_rank_forbid` policy fire correctly without requiring a
/// separate read of the User table.
fn forge_user_to_user_entity(user: &ForgeUser, store: &PolicyStore) -> Entity {
    let snapshot = store.current();
    let role_rank = snapshot.role_ranks.max_rank(&user.roles);

    let mut fields: BTreeMap<String, DynamicValue> = BTreeMap::new();
    // The User schema declares `email` as required; the legacy ForgeUser
    // record uses `username` as the canonical identifier and doesn't
    // separately track email, so we mirror it here.
    fields.insert(
        "email".to_string(),
        DynamicValue::Text(user.username.clone()),
    );
    fields.insert(
        "display_name".to_string(),
        DynamicValue::Text(
            user.display_name
                .clone()
                .unwrap_or_else(|| user.username.clone()),
        ),
    );
    fields.insert(
        "roles".to_string(),
        DynamicValue::Array(
            user.roles
                .iter()
                .cloned()
                .map(DynamicValue::Text)
                .collect(),
        ),
    );
    fields.insert("role_rank".to_string(), DynamicValue::Integer(role_rank));
    fields.insert("active".to_string(), DynamicValue::Boolean(user.active));

    // Cedar policy decisions are driven by the resource's attributes, not
    // its UID. A fresh TypeID with the `user` prefix is stable enough for
    // audit logs and avoids stitching a deterministic ID together from
    // the username.
    Entity::with_id(
        EntityId::new("user"),
        SchemaName::new("User").expect("User schema name is always valid"),
        fields,
    )
}

/// Verify the caller is allowed to grant the requested role set.
///
/// The only restricted role today is `platform_admin`: only an existing
/// platform admin may grant it. Other role names pass through. Same
/// helper will gate role edits in a future `PUT /users/:username`.
fn caller_can_grant_roles(
    claims: &Claims,
    requested_roles: &[String],
) -> Result<(), ForgeError> {
    let asks_for_platform_admin = requested_roles
        .iter()
        .any(|r| r == PLATFORM_ADMIN_ROLE);
    if asks_for_platform_admin && !claims.has_role(PLATFORM_ADMIN_ROLE) {
        return Err(ForgeError::Forbidden {
            message: format!(
                "only {PLATFORM_ADMIN_ROLE} may grant the {PLATFORM_ADMIN_ROLE} role"
            ),
        });
    }
    Ok(())
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
    /// Maximum rank across the user's `roles`, computed server-side from
    /// `role_ranks.toml`. Read-only — clients cannot set it.
    pub role_rank: i64,
}

/// Response body for `GET /users`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListUsersResponse {
    /// Users, ordered as the store returned them.
    pub users: Vec<UserResponse>,
    /// Total count of users in the store.
    pub count: usize,
}

/// One row of the `GET /users/roles` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRow {
    /// Role name as declared in `policies/role_ranks.toml`, plus the
    /// implicit `platform_admin` (only included for callers that hold it).
    pub name: String,
    /// Numeric rank from the live policy snapshot. Surfaced so admin UIs
    /// can sort or annotate without a second round-trip.
    pub rank: i64,
}

/// Response body for `GET /users/roles`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRolesResponse {
    /// Roles, ordered by rank ascending then name. `platform_admin` is
    /// omitted unless the caller already holds it (since
    /// [`caller_can_grant_roles`] would reject the grant anyway).
    pub roles: Vec<RoleRow>,
    /// Total count, equivalent to `roles.len()`.
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

/// Request body for `PUT /users/:username`.
///
/// All fields are optional — the handler interprets `Some(value)` as
/// "set to this" and `None` as "leave unchanged". An empty `roles` Vec
/// (`Some(vec![])`) is meaningful: it removes every role.
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    /// Replacement role list. `Some([])` clears all roles.
    #[serde(default)]
    pub roles: Option<Vec<String>>,
    /// Replacement display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Whether the user is active. `false` disables login.
    #[serde(default)]
    pub active: Option<bool>,
}

/// Pure helper: project a `ForgeUser` into the wire response shape.
fn user_to_response(user: &ForgeUser) -> UserResponse {
    UserResponse {
        username: user.username.clone(),
        roles: user.roles.clone(),
        display_name: user.display_name.clone(),
        active: user.active,
        role_rank: user.role_rank,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /users` — list every user.
///
/// Authorization flows through Cedar:
/// - Schema-level `ListUser` access is decided by [`check_schema_access`].
///   Without an `@access` annotation on the User schema, the secure
///   default lets only `platform_admin` (via the global permit) through —
///   matching the original handler's intent.
/// - Each row is then re-evaluated through [`authorize`] with the
///   target's synthetic User entity as resource. The global
///   `user_role_rank_forbid` policy filters out users at a higher rank
///   than the caller, so a manager-tier admin (once granted) cannot
///   enumerate platform_admin entries.
#[instrument(skip_all)]
pub async fn list_users(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;

    check_schema_access(&policy_store, &user_schema, Some(claims), AccessAction::List)?;

    let users = auth_store.list_users().await?;
    let mut responses: Vec<UserResponse> = Vec::with_capacity(users.len());
    for user in &users {
        let entity = forge_user_to_user_entity(user, policy_store.as_ref());
        let decision = authorize(
            &policy_store,
            Some(claims),
            ActionVerb::Read,
            &user_schema,
            Some(&entity),
        )
        .map_err(|e| ForgeError::Internal {
            message: format!("authz engine error during list_users: {e}"),
        })?;
        if decision.is_allow() {
            responses.push(user_to_response(user));
        }
    }
    let count = responses.len();
    Ok(Json(ListUsersResponse {
        users: responses,
        count,
    }))
}

/// `GET /users/roles` — list roles available to the caller.
///
/// Sourced from the live policy snapshot's `role_ranks` map (driven by
/// `policies/role_ranks.toml`), so admin UIs see whatever the operator
/// declared without redeploying the generated site. Authorization piggy-backs
/// on `ListUser`: anyone allowed to list users is allowed to see the role
/// catalog they'd choose from. `platform_admin` is filtered out for callers
/// that don't already hold it, because [`caller_can_grant_roles`] would
/// reject any attempt to assign it.
#[instrument(skip_all)]
pub async fn list_roles(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;

    check_schema_access(&policy_store, &user_schema, Some(claims), AccessAction::List)?;

    let snapshot = policy_store.current();
    let caller_is_platform_admin = claims.has_role(PLATFORM_ADMIN_ROLE);

    let mut rows: Vec<RoleRow> = snapshot
        .role_ranks
        .role_names()
        .filter(|name| caller_is_platform_admin || *name != PLATFORM_ADMIN_ROLE)
        .map(|name| RoleRow {
            name: name.to_string(),
            rank: snapshot.role_ranks.get(name).unwrap_or(0),
        })
        .collect();
    rows.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.name.cmp(&b.name)));

    let count = rows.len();
    Ok(Json(ListRolesResponse { roles: rows, count }))
}

/// `POST /users` — create a new user.
///
/// Schema-level access is gated by the Cedar `CreateUser` action. The
/// proposed user's effective rank (`max(role_ranks)` over `body.roles`)
/// must not exceed the caller's rank — the global `user_role_rank_forbid`
/// policy can't fire on a not-yet-created entity, so we re-evaluate it
/// here against a synthetic placeholder built from the request body.
/// Combined with [`caller_can_grant_roles`] this prevents both upward
/// rank escalation and the trivial `platform_admin` grant footgun.
#[instrument(skip_all)]
pub async fn create_user(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<CreateUserRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;

    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;

    check_schema_access(
        &policy_store,
        &user_schema,
        Some(claims),
        AccessAction::Create,
    )?;

    validate_username(&body.username)?;
    validate_password(&body.password)?;
    caller_can_grant_roles(claims, &body.roles)?;

    // Run the Cedar rank guard against a synthetic User entity carrying
    // the proposed roles. The forbid policy compares the caller's
    // role_rank with the target's; it can't fire on a placeholder
    // resource (no attributes), so we synthesize one here.
    // role_rank is recomputed inside forge_user_to_user_entity from the
    // policy_store snapshot, so this synthetic value is overwritten before
    // Cedar evaluates the request — supply a placeholder.
    let proposed = ForgeUser {
        username: body.username.clone(),
        roles: body.roles.clone(),
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
        message: format!("authz engine error during create_user: {e}"),
    })?;
    if !decision.is_allow() {
        return Err(ForgeError::Forbidden {
            message: format!(
                "creating user with roles {:?} would exceed caller's role_rank",
                body.roles
            ),
        });
    }

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

/// `DELETE /users/:username` — delete a user.
///
/// Refuses to delete the caller themselves as a defense-in-depth against
/// an operator locking themselves out mid-session. Refuses to remove the
/// last `platform_admin` (409 with `reason: "last_platform_admin"`) to
/// prevent the trivial footgun where the instance is left without
/// anyone able to manage users.
///
/// Cedar gates the actual deletion: the per-target `DeleteUser` action
/// is evaluated against the resolved User entity, so the global
/// `user_role_rank_forbid` policy stops a manager-tier admin from
/// deleting a higher-ranked user.
#[instrument(skip_all)]
pub async fn delete_user(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Path(username): Path<String>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;

    let prefixed = format!("user:{username}");
    if claims.sub == prefixed || claims.sub == username {
        return Err(ForgeError::ValidationFailed {
            details: vec!["cannot delete yourself".to_string()],
        });
    }

    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;

    let target =
        auth_store
            .get_user(&username)
            .await?
            .ok_or_else(|| ForgeError::ValidationFailed {
                details: vec![format!("user '{username}' not found")],
            })?;

    let target_entity = forge_user_to_user_entity(&target, policy_store.as_ref());
    let decision = authorize(
        &policy_store,
        Some(claims),
        ActionVerb::Delete,
        &user_schema,
        Some(&target_entity),
    )
    .map_err(|e| ForgeError::Internal {
        message: format!("authz engine error during delete_user: {e}"),
    })?;
    if !decision.is_allow() {
        return Err(ForgeError::Forbidden {
            message: format!("not authorized to delete user '{username}'"),
        });
    }

    let target_is_platform_admin =
        target.roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE);
    if target_is_platform_admin {
        let all = auth_store.list_users().await?;
        let platform_admin_count = all
            .iter()
            .filter(|u| u.roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE))
            .count();
        if platform_admin_count <= 1 {
            return Err(ForgeError::Conflict {
                reason: "last_platform_admin",
                message: format!(
                    "cannot delete '{username}': would leave instance without a {PLATFORM_ADMIN_ROLE}"
                ),
            });
        }
    }

    auth_store.delete_user(&username).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /users/:username` — update a user's roles, display name, and/or
/// active flag. Password changes go through `POST /users/:username/password`.
///
/// Cedar enforces the rank guard twice:
///
/// 1. Against the **current** target — a manager cannot edit a user who
///    already outranks them (`UpdateUser` on the resolved entity).
/// 2. Against a **synthetic post-update** target carrying the proposed
///    role set — so a caller can't promote a user to a rank they don't
///    themselves hold (same `UpdateUser` action, separate evaluation).
///
/// The "no upward escalation" rule mirrors `create_user`'s synthetic check.
/// Refuses to demote the last `platform_admin` (drop the role from the
/// only one) with `409 Conflict { reason: "last_platform_admin" }` —
/// same defense-in-depth as `delete_user`.
#[instrument(skip_all)]
pub async fn update_user(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Path(username): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<UpdateUserRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;

    let current =
        auth_store
            .get_user(&username)
            .await?
            .ok_or_else(|| ForgeError::ValidationFailed {
                details: vec![format!("user '{username}' not found")],
            })?;

    // Guard 1: caller can edit *this user as they exist now*.
    let current_entity = forge_user_to_user_entity(&current, policy_store.as_ref());
    let decision_existing = authorize(
        &policy_store,
        Some(claims),
        ActionVerb::Update,
        &user_schema,
        Some(&current_entity),
    )
    .map_err(|e| ForgeError::Internal {
        message: format!("authz engine error during update_user (existing): {e}"),
    })?;
    if !decision_existing.is_allow() {
        return Err(ForgeError::Forbidden {
            message: format!("not authorized to edit user '{username}'"),
        });
    }

    // Resolve the proposed state. `None` on a field means "keep current".
    let new_roles: Vec<String> = body
        .roles
        .clone()
        .unwrap_or_else(|| current.roles.clone());
    let new_display_name: String = body
        .display_name
        .clone()
        .or_else(|| current.display_name.clone())
        .unwrap_or_else(|| current.username.clone());
    let new_active: bool = body.active.unwrap_or(current.active);

    if body.roles.is_some() {
        caller_can_grant_roles(claims, &new_roles)?;

        // Guard 2: caller can edit *this user with the proposed roles*.
        let proposed = ForgeUser {
            username: current.username.clone(),
            roles: new_roles.clone(),
            display_name: Some(new_display_name.clone()),
            active: new_active,
            // Recomputed from the policy_store snapshot in
            // `forge_user_to_user_entity`; placeholder here.
            role_rank: 0,
        };
        let proposed_entity = forge_user_to_user_entity(&proposed, policy_store.as_ref());
        let decision_proposed = authorize(
            &policy_store,
            Some(claims),
            ActionVerb::Update,
            &user_schema,
            Some(&proposed_entity),
        )
        .map_err(|e| ForgeError::Internal {
            message: format!("authz engine error during update_user (proposed): {e}"),
        })?;
        if !decision_proposed.is_allow() {
            return Err(ForgeError::Forbidden {
                message: format!(
                    "updating '{username}' to roles {new_roles:?} would exceed caller's role_rank"
                ),
            });
        }

        // Last-platform_admin protection: refuse to demote the only one.
        let was_platform_admin =
            current.roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE);
        let still_platform_admin =
            new_roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE);
        if was_platform_admin && !still_platform_admin {
            let all = auth_store.list_users().await?;
            let count = all
                .iter()
                .filter(|u| u.roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE))
                .count();
            if count <= 1 {
                return Err(ForgeError::Conflict {
                    reason: "last_platform_admin",
                    message: format!(
                        "cannot remove {PLATFORM_ADMIN_ROLE} from '{username}': would leave instance without one"
                    ),
                });
            }
        }
    }

    // Persist. `update_user` covers roles + display_name; `toggle_user_active`
    // covers the active flag. We only call each store method when its
    // governed field actually changed, so the audit trail stays clean.
    let roles_or_name_changed =
        body.roles.is_some() || body.display_name.is_some();
    if roles_or_name_changed {
        auth_store
            .update_user(&username, &new_roles, &new_display_name)
            .await?;
    }
    if body.active.is_some() && new_active != current.active {
        auth_store.toggle_user_active(&username).await?;
    }

    let updated = auth_store
        .get_user(&username)
        .await?
        .ok_or_else(|| ForgeError::Internal {
            message: format!("updated user '{username}' not found on readback"),
        })?;
    Ok(Json(user_to_response(&updated)))
}

/// `POST /users/:username/password` — change a user's password.
///
/// Self-service is always allowed (the caller can rotate their own
/// password without needing platform_admin). For administrative
/// resets, the per-target Cedar `UpdateUser` action governs: the global
/// `user_role_rank_forbid` policy prevents a lower-ranked admin from
/// resetting a higher-ranked user's password.
#[instrument(skip_all)]
pub async fn change_password(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(auth_store): Extension<Arc<dyn DynAuthStore>>,
    Path(username): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;

    validate_password(&body.password)?;

    let target = auth_store
        .get_user(&username)
        .await?
        .ok_or_else(|| ForgeError::ValidationFailed {
            details: vec![format!("user '{username}' not found")],
        })?;

    // Self-service path: a user can always change their own password.
    let prefixed = format!("user:{username}");
    let is_self = claims.sub == prefixed || claims.sub == username;

    if !is_self {
        let user_schema = fetch_user_schema(&state).await?;
        let policy_store = fetch_policy_store(&state).await?;
        let target_entity = forge_user_to_user_entity(&target, policy_store.as_ref());
        let decision = authorize(
            &policy_store,
            Some(claims),
            ActionVerb::Update,
            &user_schema,
            Some(&target_entity),
        )
        .map_err(|e| ForgeError::Internal {
            message: format!("authz engine error during change_password: {e}"),
        })?;
        if !decision.is_allow() {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to change password for user '{username}'"),
            });
        }
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
            roles: vec!["platform_admin".to_string(), "hr".to_string()],
            display_name: Some("Alice".to_string()),
            active: true,
            role_rank: 9_223_372_036_854_775_807,
        };
        let out = user_to_response(&src);
        assert_eq!(out.username, "alice");
        assert_eq!(
            out.roles,
            vec!["platform_admin".to_string(), "hr".to_string()]
        );
        assert_eq!(out.display_name.as_deref(), Some("Alice"));
        assert!(out.active);
        assert_eq!(out.role_rank, 9_223_372_036_854_775_807);
    }

    #[test]
    fn require_auth_returns_unauthorized_when_missing() {
        let err = require_auth(&None).unwrap_err();
        assert!(matches!(err, ForgeError::Unauthorized { .. }));
    }

    #[test]
    fn caller_can_grant_roles_allows_when_caller_has_platform_admin() {
        let c = claims_with_sub("user:carol", &["platform_admin"]);
        let requested = vec!["platform_admin".to_string(), "hr".to_string()];
        assert!(caller_can_grant_roles(&c, &requested).is_ok());
    }

    #[test]
    fn caller_can_grant_roles_rejects_role_escalation() {
        let c = claims_with_sub("user:carol", &["manager"]);
        let requested = vec!["platform_admin".to_string()];
        let err = caller_can_grant_roles(&c, &requested).unwrap_err();
        assert!(matches!(err, ForgeError::Forbidden { .. }));
    }

    #[test]
    fn caller_can_grant_roles_allows_other_roles_for_non_platform_caller() {
        // A caller without platform_admin can still grant arbitrary
        // non-platform roles.
        let c = claims_with_sub("user:carol", &["manager"]);
        let requested = vec!["member".to_string(), "hr".to_string()];
        assert!(caller_can_grant_roles(&c, &requested).is_ok());
    }

    #[test]
    fn caller_can_grant_roles_allows_empty_role_list() {
        let c = claims_with_sub("user:carol", &["manager"]);
        let requested: Vec<String> = vec![];
        assert!(caller_can_grant_roles(&c, &requested).is_ok());
    }
}
