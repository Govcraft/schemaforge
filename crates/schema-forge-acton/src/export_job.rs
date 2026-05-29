//! Async export-job runner: the supervised acton-reactive actor behind the
//! `async`/over-cap/non-streamable export path of ADR-0003.
//!
//! A `POST /schemas/{schema}/entities/export` with `async: true` (or a format the
//! sync path cannot stream) does **not** block the request on materializing the
//! file. Instead the route hands an [`ExportJobSpec`] to the [`ExportJobActor`]
//! and returns a job id immediately. The actor walks the job through
//! `queued -> running -> complete | failed`, generating the CSV/NDJSON bytes via
//! the same [`materialize_export`](crate::routes::export::materialize_export)
//! pipeline the sync path uses (so the artifact inherits the identical tenant
//! scoping and per-field read stripping), writes them to the S3 storage backend
//! through the [`ExportArtifactStore`] seam, and records the object key. The
//! status endpoint later mints a TTL-bounded presigned GET URL on demand.
//!
//! The generation work runs inside the handler's `Reply::pending` future, which
//! the acton runtime owns and supervises — there is **no** `tokio::spawn`. The
//! `mutate_on` state transitions are tiny `HashMap` writes; the long-running
//! query / serialize / upload happens between them without holding the actor's
//! write lock.

use std::collections::HashMap;
use std::sync::Arc;

use acton_service::audit::{AuditLogger, AuditSeverity};
use acton_service::middleware::Claims;
use acton_service::prelude::*;
use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::types::{ExportFormat, SchemaDefinition};
use sync_wrapper::SyncFuture;
use tracing::{error, warn};

use crate::authz::PolicyStore;
use crate::messages::ReplyChannel;
use crate::routes::export::{materialize_export, ExportContext};
use crate::storage::{ExportArtifactStore, StorageRegistry};

/// Lifecycle state of an async export job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportJobStatus {
    /// Accepted and registered; generation has not started yet.
    Queued,
    /// Generation is in flight (querying / serializing / uploading).
    Running,
    /// Artifact is written to storage and ready to download.
    Complete,
    /// Generation failed; `error` on the record carries the reason.
    Failed,
}

impl ExportJobStatus {
    /// Stable lowercase wire string for the status JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

/// The actor's record of one export job. Cloned out to the status endpoint.
#[derive(Debug, Clone)]
pub struct ExportJobRecord {
    /// Opaque job id (also embedded in the object key).
    pub job_id: String,
    /// Schema the export targets; used to scope status lookups.
    pub schema_name: String,
    /// Subject (user id) that initiated the job, or `None` for an anonymous
    /// caller. Export artifacts are owner-scoped: the status endpoint only
    /// serves a record (and mints its download URL) to this same subject.
    pub owner_sub: Option<String>,
    /// Requested deliverable format.
    pub format: ExportFormat,
    /// Current lifecycle state.
    pub status: ExportJobStatus,
    /// Object-storage key of the artifact once `Complete`.
    pub object_key: Option<String>,
    /// Backend the artifact landed in; needed to presign the download URL.
    pub store: Option<Arc<dyn ExportArtifactStore>>,
    /// Row count of the finished artifact, for the audit/status payload.
    pub row_count: Option<usize>,
    /// Failure reason when `Failed`.
    pub error: Option<String>,
}

impl ExportJobRecord {
    /// Render the status payload returned by `GET .../exports/{job_id}`.
    ///
    /// When the job is `Complete`, mints a fresh TTL-bounded presigned GET URL
    /// (download links are short-lived and minted per status poll, never
    /// persisted). A presign failure degrades to status-without-url rather than
    /// failing the whole request.
    pub async fn to_status_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("job_id".into(), self.job_id.clone().into());
        obj.insert("schema".into(), self.schema_name.clone().into());
        obj.insert("format".into(), self.format.as_str().into());
        obj.insert("status".into(), self.status.as_str().into());
        if let Some(rows) = self.row_count {
            obj.insert("row_count".into(), rows.into());
        }
        if let Some(err) = &self.error {
            obj.insert("error".into(), err.clone().into());
        }
        if self.status == ExportJobStatus::Complete {
            if let (Some(store), Some(key)) = (&self.store, &self.object_key) {
                match store.presign_get(key, None).await {
                    Ok(url) => {
                        obj.insert("download_url".into(), url.into());
                    }
                    Err(e) => {
                        warn!(job_id = %self.job_id, error = %e, "failed to presign export download url");
                    }
                }
            }
        }
        serde_json::Value::Object(obj)
    }
}

/// Everything the actor needs to run one export end to end. Assembled by the
/// route handler after it has passed every authz gate, so the actor performs no
/// authorization of its own — it only materializes and uploads.
pub struct ExportJobSpec {
    pub job_id: String,
    pub schema_name: String,
    pub schema_def: SchemaDefinition,
    pub format: ExportFormat,
    pub filter: Option<serde_json::Value>,
    pub fields: Option<Vec<String>>,
    pub max_rows: u64,
    pub claims: Option<Claims>,
    pub tenant_config: Option<TenantConfig>,
    pub policy_store: Arc<PolicyStore>,
    pub record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
    /// Handle to the `ForgeActor` so the job can run queries.
    pub forge: ActorHandle,
    /// Storage backend the artifact lands in.
    pub store: Arc<dyn ExportArtifactStore>,
    /// Object key to write the artifact under.
    pub object_key: String,
    /// Audit logger for the `forge.export.completed` / `.denied` trail.
    pub audit_logger: Option<AuditLogger>,
    /// Subject (user id) for the audit trail.
    pub subject: Option<String>,
}

impl std::fmt::Debug for ExportJobSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportJobSpec")
            .field("job_id", &self.job_id)
            .field("schema_name", &self.schema_name)
            .field("format", &self.format)
            .field("object_key", &self.object_key)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Register and run an export job. Fire-and-forget: the route already returned a
/// job id to the caller, who polls the status endpoint for progress.
#[derive(Clone)]
pub struct StartExportJob {
    pub spec: Arc<ExportJobSpec>,
}

impl StartExportJob {
    /// Convenience constructor taking an owned spec.
    pub fn new(spec: ExportJobSpec) -> Self {
        Self {
            spec: Arc::new(spec),
        }
    }
}

impl std::fmt::Debug for StartExportJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartExportJob")
            .field("spec", &self.spec)
            .finish()
    }
}

/// Internal: transition a job to `Running`.
#[derive(Clone, Debug)]
struct MarkRunning {
    job_id: String,
}

/// Internal: transition a job to `Complete` with its artifact key + row count.
#[derive(Clone, Debug)]
struct CompleteExportJob {
    job_id: String,
    object_key: String,
    store: Arc<dyn ExportArtifactStore>,
    row_count: usize,
}

/// Internal: transition a job to `Failed` with a reason.
#[derive(Clone, Debug)]
struct FailExportJob {
    job_id: String,
    error: String,
}

/// Read a job record (cloned) for the status endpoint. Returns `None` when no
/// job with that id is known to this actor.
#[derive(Clone, Debug)]
pub struct GetExportJob {
    pub job_id: String,
    pub reply: ReplyChannel<Option<ExportJobRecord>>,
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Actor that owns the in-flight + completed export-job table.
///
/// State is a `job_id -> record` map. Generation runs in the `StartExportJob`
/// handler's supervised future; the brief `mutate_on` handlers only flip status
/// and stash the resulting object key, so a slow upload never blocks status
/// reads.
#[derive(Default)]
pub struct ExportJobActor {
    jobs: HashMap<String, ExportJobRecord>,
}

impl std::fmt::Debug for ExportJobActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportJobActor")
            .field("jobs", &self.jobs.len())
            .finish()
    }
}

impl ActorExtension for ExportJobActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        configure_start(actor);
        configure_transitions(actor);
        configure_reads(actor);
    }
}

fn configure_start(actor: &mut ManagedActor<Idle, ExportJobActor>) {
    actor.mutate_on::<StartExportJob>(|actor, ctx| {
        let spec = ctx.message().spec.clone();
        let self_handle = actor.handle().clone();

        // Register the job as queued before any work begins so a status poll
        // racing the generation always sees a known job.
        actor.model.jobs.insert(
            spec.job_id.clone(),
            ExportJobRecord {
                job_id: spec.job_id.clone(),
                schema_name: spec.schema_name.clone(),
                owner_sub: spec.subject.clone(),
                format: spec.format,
                status: ExportJobStatus::Queued,
                object_key: None,
                store: None,
                row_count: None,
                error: None,
            },
        );

        // Run generation in the supervised future (NOT tokio::spawn). The
        // `materialize_export` pipeline returns `Send`-only boxed futures via the
        // backend trait boundary, so wrap in `SyncFuture` to satisfy acton's
        // `Send + Sync` bound, exactly as the hook-dispatch actor does.
        Reply::pending(SyncFuture::new(async move {
            self_handle
                .send(MarkRunning {
                    job_id: spec.job_id.clone(),
                })
                .await;

            match run_job(&spec).await {
                Ok(row_count) => {
                    if let Some(logger) = &spec.audit_logger {
                        logger
                            .log_custom(
                                "forge.export.completed",
                                AuditSeverity::Informational,
                                Some(serde_json::json!({
                                    "schema": &spec.schema_name,
                                    "format": spec.format.as_str(),
                                    "filter": &spec.filter,
                                    "fields": &spec.fields,
                                    "row_count": row_count,
                                    "async": true,
                                    "job_id": &spec.job_id,
                                    "user": &spec.subject,
                                })),
                            )
                            .await;
                    }
                    self_handle
                        .send(CompleteExportJob {
                            job_id: spec.job_id.clone(),
                            object_key: spec.object_key.clone(),
                            store: spec.store.clone(),
                            row_count,
                        })
                        .await;
                }
                Err(reason) => {
                    error!(job_id = %spec.job_id, error = %reason, "async export job failed");
                    if let Some(logger) = &spec.audit_logger {
                        logger
                            .log_custom(
                                "forge.export.denied",
                                AuditSeverity::Warning,
                                Some(serde_json::json!({
                                    "schema": &spec.schema_name,
                                    "format": spec.format.as_str(),
                                    "reason": "generation_failed",
                                    "async": true,
                                    "job_id": &spec.job_id,
                                    "error": &reason,
                                    "user": &spec.subject,
                                })),
                            )
                            .await;
                    }
                    self_handle
                        .send(FailExportJob {
                            job_id: spec.job_id.clone(),
                            error: reason,
                        })
                        .await;
                }
            }
        }))
    });
}

fn configure_transitions(actor: &mut ManagedActor<Idle, ExportJobActor>) {
    actor.mutate_on::<MarkRunning>(|actor, ctx| {
        if let Some(record) = actor.model.jobs.get_mut(&ctx.message().job_id) {
            if record.status == ExportJobStatus::Queued {
                record.status = ExportJobStatus::Running;
            }
        }
        Reply::ready()
    });

    actor.mutate_on::<CompleteExportJob>(|actor, ctx| {
        let msg = ctx.message();
        if let Some(record) = actor.model.jobs.get_mut(&msg.job_id) {
            record.status = ExportJobStatus::Complete;
            record.object_key = Some(msg.object_key.clone());
            record.store = Some(msg.store.clone());
            record.row_count = Some(msg.row_count);
        }
        Reply::ready()
    });

    actor.mutate_on::<FailExportJob>(|actor, ctx| {
        let msg = ctx.message();
        if let Some(record) = actor.model.jobs.get_mut(&msg.job_id) {
            record.status = ExportJobStatus::Failed;
            record.error = Some(msg.error.clone());
        }
        Reply::ready()
    });
}

fn configure_reads(actor: &mut ManagedActor<Idle, ExportJobActor>) {
    actor.act_on::<GetExportJob>(|actor, ctx| {
        let record = actor.model.jobs.get(&ctx.message().job_id).cloned();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(record).await;
        })
    });
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Materialize the artifact and write it to storage. Returns the row count on
/// success. Any error (cap exceeded, query failure, serialization, upload) is
/// flattened to a string the actor records on the failed job.
async fn run_job(spec: &ExportJobSpec) -> std::result::Result<usize, String> {
    let ctx = ExportContext {
        forge: &spec.forge,
        claims: spec.claims.as_ref(),
        tenant_config: &spec.tenant_config,
        policy_store: &spec.policy_store,
        record_access_policy: &spec.record_access_policy,
    };
    let artifact = materialize_export(
        &ctx,
        &spec.schema_def,
        spec.format,
        spec.filter.as_ref(),
        spec.fields.as_deref(),
        spec.max_rows,
    )
    .await
    .map_err(|e| e.to_string())?;

    spec.store
        .put(&spec.object_key, artifact.bytes, artifact.content_type)
        .await
        .map_err(|e| format!("storage upload failed: {e}"))?;

    Ok(artifact.row_count)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a fresh, URL-safe export job id.
///
/// Backed by a v4 UUID (122 bits of `getrandom` CSPRNG entropy) so job ids are
/// unguessable and cannot be enumerated. This is defense-in-depth behind the
/// owner-scoped status gate: even if that authorization check ever regressed, an
/// attacker still could not discover another subject's job id by guessing.
pub fn new_job_id() -> String {
    format!("exp_{}", uuid::Uuid::new_v4().simple())
}

/// Choose the storage backend export artifacts land in. Prefers a backend named
/// `"exports"` when present, otherwise falls back to any single registered
/// backend. Returns `None` when no backend is configured.
pub fn pick_export_store(registry: &StorageRegistry) -> Option<Arc<dyn ExportArtifactStore>> {
    if let Some(client) = registry.get("exports") {
        return Some(client as Arc<dyn ExportArtifactStore>);
    }
    registry
        .single_backend()
        .map(|c| c as Arc<dyn ExportArtifactStore>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings_are_stable() {
        assert_eq!(ExportJobStatus::Queued.as_str(), "queued");
        assert_eq!(ExportJobStatus::Running.as_str(), "running");
        assert_eq!(ExportJobStatus::Complete.as_str(), "complete");
        assert_eq!(ExportJobStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn job_ids_are_unique() {
        let a = new_job_id();
        let b = new_job_id();
        assert_ne!(a, b);
        assert!(a.starts_with("exp_"));
    }

    #[tokio::test]
    async fn queued_status_json_has_no_download_url() {
        let record = ExportJobRecord {
            job_id: "exp_1".into(),
            schema_name: "Subject".into(),
            owner_sub: Some("alice".into()),
            format: ExportFormat::Csv,
            status: ExportJobStatus::Queued,
            object_key: None,
            store: None,
            row_count: None,
            error: None,
        };
        let json = record.to_status_json().await;
        assert_eq!(json["status"], "queued");
        assert_eq!(json["job_id"], "exp_1");
        assert!(json.get("download_url").is_none());
    }

    #[tokio::test]
    async fn failed_status_json_carries_error() {
        let record = ExportJobRecord {
            job_id: "exp_2".into(),
            schema_name: "Subject".into(),
            owner_sub: Some("alice".into()),
            format: ExportFormat::Ndjson,
            status: ExportJobStatus::Failed,
            object_key: None,
            store: None,
            row_count: None,
            error: Some("boom".into()),
        };
        let json = record.to_status_json().await;
        assert_eq!(json["status"], "failed");
        assert_eq!(json["error"], "boom");
        assert!(json.get("download_url").is_none());
    }
}
