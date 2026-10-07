//! Persistent, deployment-wide revocation of a subject's older bearer tokens.

use acton_service::{audit::AuditSeverity, middleware::TokenRevocation, state::AppState};
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::{
    access::{OptionalClaims, PLATFORM_ADMIN_ROLE},
    config::SchemaForgeConfig,
    error::ForgeError,
};

/// An exact token subject, bounded before it reaches persistent storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RevocationSubject(String);

impl TryFrom<String> for RevocationSubject {
    type Error = ForgeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(ForgeError::ValidationFailed {
                details: vec![
                    "subject must contain 1 to 512 bytes without control characters".into(),
                ],
            });
        }
        Ok(Self(value))
    }
}

impl From<RevocationSubject> for String {
    fn from(subject: RevocationSubject) -> Self {
        subject.0
    }
}

/// Inclusive Unix-second issuance cutoff for an exact subject.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeSubjectRequest {
    pub subject: RevocationSubject,
    pub not_before: i64,
}

/// The effective persisted cutoff, which can only increase.
#[derive(Debug, Serialize)]
pub struct RevokeSubjectResponse {
    pub subject: RevocationSubject,
    pub not_before: i64,
}

/// POST /auth/revocations. Only platform administrators can revoke a subject.
pub async fn revoke_subject(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
    Json(request): Json<RevokeSubjectRequest>,
) -> Result<Json<RevokeSubjectResponse>, ForgeError> {
    let claims = claims.ok_or_else(|| ForgeError::Unauthorized {
        message: "token revocation requires authentication".into(),
    })?;
    if !claims.has_role(PLATFORM_ADMIN_ROLE) {
        return Err(ForgeError::Forbidden {
            message: "token revocation requires platform_admin".into(),
        });
    }
    if request.not_before < 0 || chrono::DateTime::from_timestamp(request.not_before, 0).is_none() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["not_before must be a nonnegative Unix timestamp in seconds".into()],
        });
    }
    let provider = state.token_revocation().ok_or_else(unavailable)?;
    let cutoff = persist_cutoff(provider.as_ref(), &request.subject.0, request.not_before).await?;
    super::users::audit_user(
        &state,
        "forge.token.subject_revoked",
        AuditSeverity::Notice,
        &claims.sub,
        &request.subject.0,
        Some(serde_json::json!({ "not_before": cutoff })),
    )
    .await;
    Ok(Json(RevokeSubjectResponse {
        subject: request.subject,
        not_before: cutoff,
    }))
}

/// Revoke login subjects before mutating an account. A failed write must not
/// leave an erased or deactivated account with usable bearer tokens.
pub(crate) async fn revoke_account_tokens(
    state: &AppState<SchemaForgeConfig>,
    actor: &str,
    username: &str,
) -> Result<(), ForgeError> {
    let Some(provider) = state.token_revocation() else {
        // Embedders without token authentication can supply their own identity
        // layer. A configured token server must have durable revocation.
        return if state.config().token.is_some() {
            Err(unavailable())
        } else {
            Ok(())
        };
    };
    let stamp = chrono::Utc::now().timestamp();
    for subject in [format!("user:{username}"), username.to_owned()] {
        let cutoff = persist_cutoff(provider.as_ref(), &subject, stamp).await?;
        super::users::audit_user(
            state,
            "forge.token.subject_revoked",
            AuditSeverity::Notice,
            actor,
            &subject,
            Some(serde_json::json!({ "not_before": cutoff })),
        )
        .await;
    }
    Ok(())
}

async fn persist_cutoff(
    provider: &dyn TokenRevocation,
    subject: &str,
    stamp: i64,
) -> Result<i64, ForgeError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        provider.revoke_subject(subject, stamp).await?;
        provider.subject_not_before(subject).await
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "persistent token revocation failed");
        unavailable()
    })?
    .map_err(|error| {
        tracing::error!(%error, "token revocation storage operation failed");
        unavailable()
    })?
    .ok_or_else(unavailable)
}

fn unavailable() -> ForgeError {
    ForgeError::BackendUnavailable {
        message: "token revocation storage is unavailable".into(),
    }
}
