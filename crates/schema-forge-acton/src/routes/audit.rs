//! Deployment-wide audit access. Only platform administrators may read or verify.

use std::sync::Arc;
use std::time::Duration;

use acton_service::audit::{AuditEvent, AuditStorage};
use acton_service::middleware::Claims;
use acton_service::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::access::{OptionalClaims, PLATFORM_ADMIN_ROLE};
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;

const DEADLINE: Duration = Duration::from_secs(5);
const MAX_PAGE: usize = 200;
const MAX_VERIFY: u64 = 1_000;

/// Dedicated deployment-level audit permission, shared by routes and discovery.
pub fn can_access_audit(claims: &Claims) -> bool {
    claims.has_role(PLATFORM_ADMIN_ROLE)
}

fn require_audit(claims: Option<&Claims>) -> Result<(), ForgeError> {
    let claims = claims.ok_or_else(|| ForgeError::Unauthorized {
        message: "audit access requires authentication".into(),
    })?;
    if can_access_audit(claims) {
        Ok(())
    } else {
        Err(ForgeError::Forbidden {
            message: "deployment-wide audit access requires platform_admin".into(),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Availability {
    Disabled,
    Unavailable,
    Empty,
    Available,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct SequenceRange {
    from_sequence: u64,
    to_sequence: u64,
}

#[derive(Debug, Serialize)]
struct CollectionStatus {
    configured: bool,
    logger_active: bool,
    all_http_requests: bool,
    auth_events: bool,
    config_events: bool,
    audited_routes: Vec<String>,
    excluded_routes: Vec<String>,
    mutation_success_acknowledges_persistence: bool,
}

#[derive(Debug, Serialize)]
struct AuditStatus {
    availability: Availability,
    scope: &'static str,
    required_role: &'static str,
    collection: CollectionStatus,
    retained_range: Option<SequenceRange>,
    retention_days: Option<u32>,
    observed_at: DateTime<Utc>,
    max_page_size: usize,
    max_verification_events: u64,
    order: &'static str,
    filters: [&'static str; 3],
    verification_trust: &'static str,
}

fn active_storage(state: &AppState<SchemaForgeConfig>) -> Option<&Arc<dyn AuditStorage>> {
    if !state
        .config()
        .audit
        .as_ref()
        .is_none_or(|config| config.enabled)
    {
        return None;
    }
    state.audit_storage()
}

/// GET /audit/status. Distinguishes disabled collection, unavailable storage and emptiness.
pub async fn status(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<Response, ForgeError> {
    require_audit(claims.as_ref())?;
    let config = state.config().audit.clone().unwrap_or_default();
    let configured = config.enabled;
    let (availability, retained_range) = if !configured {
        (Availability::Disabled, None)
    } else if let Some(storage) = active_storage(&state) {
        match tokio::time::timeout(DEADLINE, storage.sequence_bounds()).await {
            Ok(Ok(Some((from_sequence, to_sequence)))) => (
                Availability::Available,
                Some(SequenceRange {
                    from_sequence,
                    to_sequence,
                }),
            ),
            Ok(Ok(None)) => (Availability::Empty, None),
            _ => (Availability::Unavailable, None),
        }
    } else {
        (Availability::Unavailable, None)
    };
    Ok(Json(AuditStatus {
        availability,
        scope: "deployment",
        required_role: PLATFORM_ADMIN_ROLE,
        collection: CollectionStatus {
            configured,
            logger_active: state.audit_logger().is_some(),
            all_http_requests: configured && config.audit_all_requests,
            auth_events: configured && config.audit_auth_events,
            config_events: configured && config.audit_config_events,
            audited_routes: config.audited_routes,
            excluded_routes: config.excluded_routes,
            mutation_success_acknowledges_persistence: false,
        },
        retained_range,
        retention_days: config.retention_days,
        observed_at: Utc::now(),
        max_page_size: MAX_PAGE,
        max_verification_events: MAX_VERIFY,
        order: "sequence_ascending",
        filters: ["after_sequence", "through_sequence", "limit"],
        verification_trust: "local_stored_anchor_only",
    })
    .into_response())
}

/// Only sequence bounds and page size are supported; unknown filters are rejected.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    /// Exclusive cursor returned by the previous page.
    pub after_sequence: Option<u64>,
    /// Fixed inclusive upper sequence, returned by the first page.
    pub through_sequence: Option<u64>,
    /// Requested page size, 1 through 200 (default 100).
    pub limit: Option<usize>,
}

/// Approved projection. Request paths, source details and metadata are intentionally absent.
#[derive(Debug, Serialize)]
struct EventView {
    id: String,
    sequence: u64,
    timestamp: DateTime<Utc>,
    kind: String,
    severity: String,
    service_name: String,
    hash: Option<String>,
    previous_hash: Option<String>,
}

impl From<AuditEvent> for EventView {
    fn from(event: AuditEvent) -> Self {
        Self {
            id: event.id.to_string(),
            sequence: event.sequence,
            timestamp: event.timestamp,
            kind: event.kind.to_string(),
            severity: event.severity.to_string(),
            service_name: event.service_name,
            hash: event.hash,
            previous_hash: event.previous_hash,
        }
    }
}

#[derive(Debug, Serialize)]
struct EventsPage {
    events: Vec<EventView>,
    through_sequence: u64,
    next_after_sequence: Option<u64>,
    retained_range: Option<SequenceRange>,
    observed_at: DateTime<Utc>,
}

fn invalid(message: &str) -> ForgeError {
    ForgeError::InvalidQuery {
        message: message.into(),
    }
}

fn unavailable() -> ForgeError {
    ForgeError::BackendUnavailable {
        message: "audit storage unavailable or request deadline exceeded".into(),
    }
}

fn snapshot_incomplete() -> ForgeError {
    ForgeError::Conflict {
        reason: "audit_snapshot_incomplete",
        message: "requested audit snapshot contains unavailable sequences; restart browsing or investigate retention and collection gaps".into(),
    }
}

/// GET /audit/events. A fixed upper sequence excludes later appends from subsequent pages.
pub async fn events(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
    query: Result<Query<EventsQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ForgeError> {
    require_audit(claims.as_ref())?;
    let Query(query) = query.map_err(|_| invalid("invalid audit query parameters"))?;
    let limit = query.limit.unwrap_or(100);
    if limit == 0 || limit > MAX_PAGE {
        return Err(invalid("limit must be between 1 and 200"));
    }
    if query.after_sequence.is_some() && query.through_sequence.is_none() {
        return Err(invalid("through_sequence is required with after_sequence"));
    }
    let storage = active_storage(&state).ok_or_else(unavailable)?;
    let page = tokio::time::timeout(DEADLINE, read_page(storage.as_ref(), query, limit))
        .await
        .map_err(|_| unavailable())??;
    Ok(Json(page).into_response())
}

async fn read_page(
    storage: &dyn AuditStorage,
    query: EventsQuery,
    limit: usize,
) -> Result<EventsPage, ForgeError> {
    let bounds = storage.sequence_bounds().await.map_err(|_| unavailable())?;
    let (first, latest) = bounds.unwrap_or((1, 0));
    let through = query.through_sequence.unwrap_or(latest);
    let after = query.after_sequence.unwrap_or(first.saturating_sub(1));
    if through > latest || (through > 0 && through < first) {
        return Err(snapshot_incomplete());
    }
    if after > through && through != 0 {
        return Err(invalid("after_sequence must not exceed through_sequence"));
    }
    if through == 0 && after != 0 {
        return Err(invalid("empty snapshot requires after_sequence zero"));
    }
    let mut events = Vec::new();
    let mut next_after_sequence = None;
    if after < through {
        if after.saturating_add(1) < first {
            return Err(snapshot_incomplete());
        }
        let fetched = storage
            .query_sequence(after + 1, through, limit)
            .await
            .map_err(|_| unavailable())?;
        let mut expected = after + 1;
        for event in &fetched {
            if event.sequence != expected || event.sequence > through {
                return Err(snapshot_incomplete());
            }
            expected = expected.saturating_add(1);
        }
        let last = fetched
            .last()
            .map(|event| event.sequence)
            .ok_or_else(snapshot_incomplete)?;
        if fetched.len() > limit || (fetched.len() < limit && last < through) {
            return Err(snapshot_incomplete());
        }
        if last < through {
            next_after_sequence = Some(last);
        }
        events = fetched.into_iter().map(EventView::from).collect();
    }
    Ok(EventsPage {
        events,
        through_sequence: through,
        next_after_sequence,
        retained_range: bounds.map(|(from_sequence, to_sequence)| SequenceRange {
            from_sequence,
            to_sequence,
        }),
        observed_at: Utc::now(),
    })
}

/// Inclusive bounded verification request; a missing range is never inferred as valid.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyRequest {
    /// First requested sequence (genesis is 1).
    pub from_sequence: u64,
    /// Last requested sequence, inclusive.
    pub to_sequence: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum VerificationOutcome {
    Valid,
    Broken,
    Incomplete,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct VerificationAnchor {
    kind: &'static str,
    sequence: u64,
    independently_trusted: bool,
}

#[derive(Debug, Serialize)]
struct VerificationResponse {
    outcome: VerificationOutcome,
    requested_range: SequenceRange,
    checked_range: Option<SequenceRange>,
    anchor: VerificationAnchor,
    broken_sequence: Option<u64>,
    observed_at: DateTime<Utc>,
    establishes: &'static str,
}

/// POST /audit/verify. Checks at most 1,000 events and their immediate predecessor.
pub async fn verify(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
    body: Result<Json<VerifyRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ForgeError> {
    use acton_service::audit::storage::AuditVerification;

    require_audit(claims.as_ref())?;
    let Json(request) =
        body.map_err(|_| invalid("expected from_sequence and to_sequence as JSON integers"))?;
    if request.from_sequence == 0
        || request.to_sequence < request.from_sequence
        || request.to_sequence - request.from_sequence >= MAX_VERIFY
    {
        return Err(invalid(
            "verification requires 1 <= from_sequence <= to_sequence and at most 1000 events",
        ));
    }
    let result = if let Some(storage) = active_storage(&state) {
        tokio::time::timeout(
            DEADLINE,
            storage.verify_chain_range(request.from_sequence, request.to_sequence),
        )
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        None
    };
    let requested_range = SequenceRange {
        from_sequence: request.from_sequence,
        to_sequence: request.to_sequence,
    };
    let (outcome, checked_range, broken_sequence) = match result {
        Some(AuditVerification::Consistent) => {
            (VerificationOutcome::Valid, Some(requested_range), None)
        }
        Some(AuditVerification::Broken { sequence }) => (
            VerificationOutcome::Broken,
            (sequence >= request.from_sequence).then_some(SequenceRange {
                from_sequence: request.from_sequence,
                to_sequence: sequence,
            }),
            Some(sequence),
        ),
        Some(AuditVerification::Incomplete) => (VerificationOutcome::Incomplete, None, None),
        None => (VerificationOutcome::Unavailable, None, None),
    };
    let status = if matches!(outcome, VerificationOutcome::Unavailable) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(VerificationResponse {
            outcome,
            requested_range,
            checked_range,
            anchor: VerificationAnchor {
                kind: if request.from_sequence == 1 {
                    "genesis"
                } else {
                    "stored_predecessor"
                },
                sequence: request.from_sequence - 1,
                independently_trusted: false,
            },
            broken_sequence,
            observed_at: Utc::now(),
            establishes: "local_chain_consistency_only_not_collection_coverage_or_external_trust",
        }),
    )
        .into_response())
}
