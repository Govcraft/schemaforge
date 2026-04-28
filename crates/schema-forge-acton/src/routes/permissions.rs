//! Admin-shell permission summary: `GET /api/v1/forge/permissions`.
//!
//! Returns a tiny envelope of boolean flags the React admin shell uses to
//! decide which top-level admin nav items to render. Per-schema and
//! per-entity decisions live on the schemas/entities responses where they
//! belong; this endpoint covers the *non-entity* admin sections (Schemas
//! catalog, Users) where there's no obvious resource to attach flags to.
//!
//! Authorization model:
//! - `schemas_manage`: only `platform_admin`. Schema CRUD is platform-wide
//!   and gates on `require_admin` in `routes::schemas`; mirroring that here
//!   keeps the nav and the API in lockstep.
//! - `users_manage`: anyone Cedar permits to `List` the User schema.
//!   Matches the gate on `routes::users::list_users`, so the Users nav
//!   item only appears when the underlying page would actually load.

use std::sync::Arc;

use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use schema_forge_core::types::SchemaDefinition;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::instrument;

use crate::access::{check_schema_access, AccessAction, OptionalClaims, PLATFORM_ADMIN_ROLE};
use crate::actor::ForgeActor;
use crate::authz::PolicyStore;
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;
use crate::messages::{GetPolicyStore, GetSchema, ReplyChannel};

/// Admin-shell permission flags the React sidebar consumes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AdminPermissions {
    /// Whether the Schemas admin section should be shown.
    pub schemas_manage: bool,
    /// Whether the Users admin section should be shown.
    pub users_manage: bool,
}

/// Top-level response envelope, structured to leave room for non-admin
/// permission groups in future revisions without breaking clients.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PermissionsResponse {
    pub admin: AdminPermissions,
}

/// `GET /permissions` — return the admin-shell flags for the caller.
#[instrument(skip_all)]
pub async fn get_permissions(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = claims.ok_or(ForgeError::Unauthorized {
        message: "authentication required".to_string(),
    })?;

    let schemas_manage = claims.has_role(PLATFORM_ADMIN_ROLE);

    // `users_manage` defers to the same Cedar gate the Users page itself
    // runs, so the nav item never appears for callers who'd hit a 403 on
    // the underlying list. Missing User schema is a programmer error, not
    // a user-facing condition; surface as Internal so it's traceable.
    let user_schema = fetch_user_schema(&state).await?;
    let policy_store = fetch_policy_store(&state).await?;
    let users_manage = check_schema_access(
        &policy_store,
        &user_schema,
        Some(&claims),
        AccessAction::List,
    )
    .is_ok();

    Ok(Json(PermissionsResponse {
        admin: AdminPermissions {
            schemas_manage,
            users_manage,
        },
    }))
}

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
            message: "Cedar policy store not initialized".into(),
        })
}

