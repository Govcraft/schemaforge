//! Deployment-wide audit access. Only platform administrators may read or verify.

use std::sync::Arc;
use std::time::Duration;

use acton_service::audit::storage::{AuditOrder, AuditQuery};
use acton_service::audit::{AuditEvent, AuditEventKind, AuditSeverity, AuditStorage};
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
    investigation_available: bool,
    investigation_order: &'static str,
    investigation_filters: Vec<&'static str>,
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
        investigation_available: true,
        investigation_order: "newest",
        investigation_filters: vec![
            "from",
            "to",
            "kind",
            "severity",
            "subject",
            "actor",
            "request_id",
            "schema",
            "entity_id",
            "status_code",
            "cursor",
            "through_sequence",
            "limit",
        ],
        verification_trust: "local_stored_anchor_only",
    })
    .into_response())
}

/// Legacy sequence browsing plus an explicit filtered investigation mode.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    /// Exclusive cursor returned by the previous page.
    pub after_sequence: Option<u64>,
    /// Fixed inclusive upper sequence, returned by the first page.
    pub through_sequence: Option<u64>,
    /// Requested page size, 1 through 200 (default 100).
    pub limit: Option<usize>,
    /// Opt in to filtered newest-first investigation; omitted keeps legacy order.
    pub order: Option<String>,
    /// Exclusive descending sequence cursor.
    pub cursor: Option<u64>,
    /// Inclusive timestamp lower bound.
    pub from: Option<DateTime<Utc>>,
    /// Inclusive timestamp upper bound.
    pub to: Option<DateTime<Utc>>,
    /// Exact event kind as displayed in events.
    pub kind: Option<String>,
    /// Syslog severity name, case insensitive.
    pub severity: Option<String>,
    /// Exact source subject.
    pub subject: Option<String>,
    /// Exact source subject or recognized forge actor/user.
    pub actor: Option<String>,
    /// Exact request correlation identifier.
    pub request_id: Option<String>,
    /// Exact schema name on recognized forge events.
    pub schema: Option<String>,
    /// Exact entity identifier on recognized forge events.
    pub entity_id: Option<String>,
    /// Exact HTTP status code.
    pub status_code: Option<u16>,
}

/// Approved evidence projection; arbitrary metadata and request query strings never leave storage.
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
    subject: Option<String>,
    request_id: Option<String>,
    method: Option<String>,
    path: Option<String>,
    status_code: Option<u16>,
    duration_ms: Option<u64>,
    schema: Option<String>,
    entity_id: Option<String>,
    user: Option<String>,
    actor: Option<String>,
    target: Option<String>,
    changed_fields: Vec<String>,
    reason: Option<String>,
    tenant_id: Option<String>,
    previous_roles: Option<Vec<String>>,
    new_roles: Option<Vec<String>>,
    active: Option<bool>,
    self_service: Option<bool>,
}

impl From<AuditEvent> for EventView {
    fn from(event: AuditEvent) -> Self {
        let metadata = known_forge_event(&event.kind)
            .then_some(event.metadata.as_ref())
            .flatten();
        let field = |name: &str| {
            metadata
                .and_then(|v| v.get(name))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        let changed_fields = metadata
            .and_then(|v| v.get("changed_fields"))
            .and_then(|v| v.as_array())
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let is_kind =
            |name: &str| matches!(&event.kind, AuditEventKind::Custom(kind) if kind == name);
        let roles = |name: &str| -> Option<Vec<String>> {
            metadata?
                .get(name)?
                .as_array()?
                .iter()
                .map(|role| role.as_str().map(str::to_owned))
                .collect()
        };
        let previous_roles = is_kind("forge.user.updated")
            .then(|| roles("prev_roles"))
            .flatten();
        let new_roles = is_kind("forge.user.updated")
            .then(|| roles("new_roles"))
            .flatten();
        let active = is_kind("forge.user.active_toggled")
            .then(|| metadata?.get("active")?.as_bool())
            .flatten();
        let self_service = is_kind("forge.user.password_changed")
            .then(|| metadata?.get("self_service")?.as_bool())
            .flatten();
        let user = field("user");
        let actor = field("actor").or_else(|| user.clone());
        Self {
            previous_roles,
            new_roles,
            active,
            self_service,
            schema: field("schema"),
            entity_id: field("entity_id"),
            user,
            actor,
            target: field("target"),
            changed_fields,
            reason: field("reason").filter(|reason| {
                matches!(
                    reason.as_str(),
                    "record_can_modify"
                        | "record_can_delete"
                        | "schema_access"
                        | "record_access"
                        | "record_visibility"
                        | "role_rank_guard"
                        | "file_access"
                        | "platform_admin_required"
                        | "export_not_enabled"
                        | "format_not_allowed"
                        | "authz_denied"
                        | "no_exportable_fields_requested"
                        | "rate_limited"
                        | "row_cap_exceeded"
                        | "generation_failed"
                        | "expired"
                        | "not_pending"
                )
            }),
            tenant_id: field("tenant_id"),
            request_id: event.source.request_id.or_else(|| field("request_id")),
            subject: event.source.subject,
            method: event.method,
            path: event
                .path
                .and_then(|p| p.split(['?', '#']).next().map(str::to_owned)),
            status_code: event.status_code,
            duration_ms: event.duration_ms,
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

const KNOWN_FORGE_EVENTS: &[&str] = &[
    "forge.entity.created",
    "forge.entity.updated",
    "forge.entity.patched",
    "forge.entity.deleted",
    "forge.access.denied",
    "forge.schema.created",
    "forge.schema.migrated",
    "forge.schema.deleted",
    "forge.user.created",
    "forge.user.updated",
    "forge.user.deleted",
    "forge.user.active_toggled",
    "forge.user.password_changed",
    "forge.invite.created",
    "forge.invite.accepted",
    "forge.invite.rejected",
    "forge.invite.send_failed",
    "forge.file.upload_minted",
    "forge.file.uploaded",
    "forge.file.detached",
    "forge.file.scan_complete",
    "forge.file.downloaded",
    "forge.export.denied",
    "forge.export.initiated",
    "forge.export.completed",
];

fn known_forge_event(kind: &AuditEventKind) -> bool {
    matches!(kind, AuditEventKind::Custom(name) if KNOWN_FORGE_EVENTS.contains(&name.as_str()))
}

#[derive(Debug, Serialize)]
struct EventsPage {
    events: Vec<EventView>,
    through_sequence: u64,
    next_after_sequence: Option<u64>,
    order: &'static str,
    next_cursor: Option<u64>,
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
    if query.order.as_deref() == Some("newest") {
        let storage = active_storage(&state).ok_or_else(unavailable)?;
        let page =
            tokio::time::timeout(DEADLINE, read_investigation(storage.as_ref(), query, limit))
                .await
                .map_err(|_| unavailable())??;
        return Ok(Json(page).into_response());
    }
    if query.order.is_some()
        || query.cursor.is_some()
        || query.from.is_some()
        || query.to.is_some()
        || query.kind.is_some()
        || query.severity.is_some()
        || query.subject.is_some()
        || query.actor.is_some()
        || query.request_id.is_some()
        || query.schema.is_some()
        || query.entity_id.is_some()
        || query.status_code.is_some()
    {
        return Err(invalid("investigation filters require order=newest"));
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
        order: "oldest",
        next_cursor: None,
        retained_range: bounds.map(|(from_sequence, to_sequence)| SequenceRange {
            from_sequence,
            to_sequence,
        }),
        observed_at: Utc::now(),
    })
}

fn parse_severity(value: &str) -> Result<AuditSeverity, ForgeError> {
    match value.to_ascii_uppercase().as_str() {
        "EMERGENCY" => Ok(AuditSeverity::Emergency),
        "ALERT" => Ok(AuditSeverity::Alert),
        "CRITICAL" => Ok(AuditSeverity::Critical),
        "ERROR" => Ok(AuditSeverity::Error),
        "WARNING" => Ok(AuditSeverity::Warning),
        "NOTICE" => Ok(AuditSeverity::Notice),
        "INFO" | "INFORMATIONAL" => Ok(AuditSeverity::Informational),
        "DEBUG" => Ok(AuditSeverity::Debug),
        _ => Err(invalid("unsupported audit severity")),
    }
}

async fn read_investigation(
    storage: &dyn AuditStorage,
    query: EventsQuery,
    limit: usize,
) -> Result<EventsPage, ForgeError> {
    if query.after_sequence.is_some()
        || (query.cursor.is_some() && query.through_sequence.is_none())
    {
        return Err(invalid(
            "newest browsing uses cursor with through_sequence, not after_sequence",
        ));
    }
    if query.from.zip(query.to).is_some_and(|(from, to)| from > to) {
        return Err(invalid("from must not exceed to"));
    }
    if query
        .status_code
        .is_some_and(|status| !(100..=599).contains(&status))
    {
        return Err(invalid("status_code must be between 100 and 599"));
    }
    let kind = query
        .kind
        .as_deref()
        .map(|kind| {
            AuditEventKind::from_wire(kind).ok_or_else(|| invalid("unsupported audit event kind"))
        })
        .transpose()?;
    let severity = query.severity.as_deref().map(parse_severity).transpose()?;
    let bounds = storage.sequence_bounds().await.map_err(|_| unavailable())?;
    let (first, latest) = bounds.unwrap_or((1, 0));
    let through = query.through_sequence.unwrap_or(latest);
    if through > latest || (through > 0 && through < first) {
        return Err(snapshot_incomplete());
    }
    if query
        .cursor
        .is_some_and(|cursor| cursor == 0 || cursor > through)
    {
        return Err(invalid("cursor must be within the snapshot"));
    }
    if query.cursor.is_some_and(|cursor| cursor < first) {
        return Err(snapshot_incomplete());
    }
    let storage_query = AuditQuery {
        metadata_kinds: Some(
            KNOWN_FORGE_EVENTS
                .iter()
                .map(|name| format!("custom.{name}"))
                .collect(),
        ),
        from: query.from,
        to: query.to,
        kind,
        severity,
        subject: query.subject,
        actor: query.actor,
        request_id: query.request_id,
        schema: query.schema,
        entity_id: query.entity_id,
        status_code: query.status_code,
        cursor: query.cursor,
        through_sequence: Some(through),
        order: AuditOrder::NewestFirst,
        limit: limit + 1,
        ..AuditQuery::default()
    };
    let mut fetched = storage
        .query_filtered(&storage_query)
        .await
        .map_err(|_| unavailable())?;
    if fetched.len() > limit + 1
        || fetched
            .windows(2)
            .any(|pair| pair[0].sequence <= pair[1].sequence)
        || fetched.iter().any(|event| {
            event.sequence > through || query.cursor.is_some_and(|cursor| event.sequence >= cursor)
        })
    {
        return Err(unavailable());
    }
    let more = fetched.len() > limit;
    fetched.truncate(limit);
    let next_cursor = more
        .then(|| fetched.last().map(|event| event.sequence))
        .flatten();
    Ok(EventsPage {
        events: fetched.into_iter().map(EventView::from).collect(),
        through_sequence: through,
        next_after_sequence: None,
        order: "newest",
        next_cursor,
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

/// Emit a known forge event with identity and correlation, never request bodies.
pub(super) async fn log_forge_event(
    state: &AppState<SchemaForgeConfig>,
    claims: Option<&Claims>,
    headers: &axum::http::HeaderMap,
    name: &str,
    severity: AuditSeverity,
    mut metadata: serde_json::Value,
) {
    let Some(logger) = state.audit_logger() else {
        return;
    };
    let subject = claims.map(|claims| claims.sub.clone());
    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(fields) = metadata.as_object_mut() {
        fields.insert("actor".into(), serde_json::json!(subject));
        fields.insert("request_id".into(), serde_json::json!(request_id));
        let tenant = claims
            .filter(|claims| !claims.has_role(PLATFORM_ADMIN_ROLE))
            .and_then(|claims| claims.custom.get("tenant_chain"))
            .and_then(|chain| chain.as_array())
            .and_then(|chain| chain.first())
            .and_then(|tenant| tenant.get("entity_id"))
            .and_then(|id| id.as_str());
        fields.insert("tenant_id".into(), serde_json::json!(tenant));
    }
    let mut event = AuditEvent::new(
        AuditEventKind::Custom(name.into()),
        severity,
        logger.service_name().into(),
    );
    event.source.subject = subject;
    event.source.request_id = request_id;
    event.metadata = Some(metadata);
    logger.log(event).await;
}

/// Trusted request context for application authentication audit events.
pub struct RequestAuditSource(pub acton_service::audit::AuditSource);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for RequestAuditSource {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let source = parts
            .extensions
            .get::<acton_service::middleware::request_context::RequestContext>()
            .map(|context| context.audit_source())
            .unwrap_or_else(|| acton_service::audit::AuditSource {
                request_id: parts
                    .headers
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                ..acton_service::audit::AuditSource::default()
            });
        Ok(Self(source))
    }
}

#[cfg(test)]
mod investigation_tests {
    use super::*;
    use axum::extract::FromRequestParts;

    #[tokio::test]
    async fn authentication_source_prefers_resolved_context_over_untrusted_headers() {
        let (mut parts, ()) = axum::http::Request::builder()
            .header("x-request-id", "header-request")
            .header("x-forwarded-for", "203.0.113.77")
            .body(())
            .unwrap()
            .into_parts();
        parts
            .extensions
            .insert(acton_service::middleware::request_context::RequestContext {
                request_id: Some("resolved-request".into()),
                ip: Some("127.0.0.1".parse().unwrap()),
                user_agent: None,
            });
        let RequestAuditSource(source) = RequestAuditSource::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(source.request_id.as_deref(), Some("resolved-request"));
        assert_eq!(source.ip.as_deref(), Some("127.0.0.1"));
        parts.extensions.clear();
        let RequestAuditSource(source) = RequestAuditSource::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(source.request_id.as_deref(), Some("header-request"));
        assert!(source.ip.is_none());
    }

    #[test]
    fn scanner_free_text_is_not_an_approved_reason() {
        let mut event = AuditEvent::new(
            AuditEventKind::Custom("forge.file.scan_complete".into()),
            AuditSeverity::Notice,
            "synthetic".into(),
        );
        event.metadata =
            Some(serde_json::json!({"reason":"SECRET scanner body", "schema":"Record"}));
        let view = EventView::from(event);
        assert!(view.reason.is_none());
        assert_eq!(view.schema.as_deref(), Some("Record"));
    }
}
