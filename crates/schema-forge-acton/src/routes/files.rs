//! HTTP handlers for `file` field uploads and downloads.
//!
//! Three endpoints per file field:
//!
//! - `POST /schemas/{schema}/entities/{id}/fields/{field}/upload-url` — mint a
//!   presigned PUT URL for the client to upload bytes directly to S3.
//! - `POST /schemas/{schema}/entities/{id}/fields/{field}/confirm-upload` —
//!   verify the upload landed, record the attachment on the entity, and fire
//!   the `after_upload` hook.
//! - `GET  /schemas/{schema}/entities/{id}/fields/{field}` — serve the file,
//!   either by redirecting to a short-lived presigned GET (`access: :presigned`)
//!   or by streaming bytes through the runtime (`access: :proxied`).
//!
//! The runtime never sees upload bytes. Downloads in presigned mode also skip
//! the runtime entirely; only proxied mode streams through it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use chrono::Utc;
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    DynamicValue, EntityId, FieldType, FileAccess, FileAttachment, FileConstraints, FileStatus,
    HookEvent, SchemaDefinition, SchemaName,
};
use std::str::FromStr;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::{debug, instrument};
use uuid::Uuid;

use crate::access::{check_schema_access, AccessAction, OptionalClaims, PLATFORM_ADMIN_ROLE};
use crate::actor::ForgeActor;
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;
use crate::hooks::{
    run_before_hook, DispatchHook, HookDispatchActor, HookDispatcher, HookInvocation, HooksConfig,
};
use crate::messages::{
    GetEntity, GetHookDispatcher, GetSchema, GetStorageRegistry, ReplyChannel, UpdateEntity,
};
use crate::storage::{S3Client, StorageRegistry};

/// Timeout for actor round-trips. Matches the entities route module so latency
/// budgets stay aligned across the runtime.
const ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Request body for `POST /.../upload-url`.
#[derive(Debug, Deserialize)]
pub struct MintUploadUrlRequest {
    /// Client-reported filename. Used only as a sanitized suffix in the object
    /// key so operators can identify files in the bucket; not trusted for
    /// validation.
    pub filename: String,
    /// Asserted MIME type. Must match the field's allowlist. The client MUST
    /// send this exact value as the `Content-Type` header on the subsequent PUT
    /// or S3 will reject the upload.
    pub mime: String,
    /// Asserted byte size. Must not exceed the field's `max_size`. Verified
    /// independently by `HeadObject` on confirm.
    pub size: u64,
}

/// Response body for `POST /.../upload-url`.
#[derive(Debug, Serialize)]
pub struct MintUploadUrlResponse {
    /// Fully-qualified URL the client PUTs bytes to.
    pub upload_url: String,
    /// Server-generated object key. The client echoes this back in
    /// `confirm-upload` so the server knows which pending attachment to validate.
    pub key: String,
    /// Headers the client MUST include on the PUT request.
    pub headers: BTreeMap<String, String>,
    /// Wall-clock time the presigned URL expires (ISO 8601).
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Request body for `POST /.../confirm-upload`.
#[derive(Debug, Deserialize)]
pub struct ConfirmUploadRequest {
    /// Object key previously returned from `upload-url`.
    pub key: String,
    /// Optional hex-encoded SHA-256 checksum the client computed before upload.
    /// Stored alongside the attachment for forensic use; not re-verified by the
    /// runtime (that would require a full byte scan).
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

/// Response body for `POST /.../confirm-upload` and downstream representations.
#[derive(Debug, Serialize)]
pub struct AttachmentResponse {
    /// Lifecycle status after the confirm completed.
    pub status: String,
    /// Attachment snapshot, shape-compatible with `FileAttachment`.
    pub attachment: FileAttachment,
}

/// Query parameters for `GET /.../fields/{field}`.
#[derive(Debug, Deserialize, Default)]
pub struct DownloadQuery {
    /// When `false`, presigned-mode returns `{ "url": "..." }` instead of a 302
    /// redirect. Useful for SPAs that want to mint an href without the browser
    /// following it.
    #[serde(default)]
    pub redirect: Option<bool>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Mint a presigned PUT URL for a specific `file` field on an existing entity.
#[instrument(skip(state, claims, body), fields(schema, entity_id))]
pub async fn mint_upload_url(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, entity_id, field)): Path<(String, String, String)>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<MintUploadUrlRequest>,
) -> Result<Json<MintUploadUrlResponse>, ForgeError> {
    let ctx = load_file_context(&state, &schema, &entity_id, &field, claims.as_ref()).await?;
    check_schema_access(&ctx.schema, claims.as_ref(), AccessAction::Write)?;

    validate_upload_request(&body, &ctx.constraints)?;

    let client = resolve_backend(&ctx.registry, &ctx.constraints.bucket)?;
    let key = build_object_key(
        &ctx.schema.name,
        &ctx.entity.id,
        &field,
        &body.filename,
        tenant_prefix(&ctx.entity),
    );

    // Fire `before_upload` (blocking). The hook may abort the mint (e.g. a
    // tighter MIME policy than the DSL expresses) by returning Aborted.
    run_file_before_hook(
        &state,
        &ctx.schema,
        HookEvent::BeforeUpload,
        "mint_upload_url",
        claims.as_ref(),
        Some(ctx.entity.id.as_str().to_string()),
        file_hook_fields(
            &field,
            &body.filename,
            &body.mime,
            body.size,
            None,
            None,
            None,
        ),
    )
    .await?;

    let presigned = client
        .presign_put(&key, &body.mime, None)
        .await
        .map_err(|e| ForgeError::Internal {
            message: format!("failed to presign upload for bucket '{}': {e}", ctx.constraints.bucket),
        })?;

    let expires_at = Utc::now() + chrono::Duration::seconds(presigned.expires_in_secs as i64);
    let mut headers = BTreeMap::new();
    headers.insert("Content-Type".to_string(), presigned.content_type);

    debug!(
        schema = %ctx.schema.name,
        entity = %ctx.entity.id,
        field = %field,
        key = %presigned.key,
        "minted upload url"
    );

    Ok(Json(MintUploadUrlResponse {
        upload_url: presigned.url,
        key: presigned.key,
        headers,
        expires_at,
    }))
}

/// Verify a claimed upload and persist a `FileAttachment` onto the entity.
#[instrument(skip(state, claims, body), fields(schema, entity_id))]
pub async fn confirm_upload(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, entity_id, field)): Path<(String, String, String)>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<ConfirmUploadRequest>,
) -> Result<Json<AttachmentResponse>, ForgeError> {
    let ctx = load_file_context(&state, &schema, &entity_id, &field, claims.as_ref()).await?;
    check_schema_access(&ctx.schema, claims.as_ref(), AccessAction::Write)?;

    // Reject keys that don't match the expected per-entity/per-field prefix.
    // Prevents a caller from attaching a confirmed object that was uploaded
    // under a different entity's presigned URL.
    let expected_prefix = format!(
        "{}/{}/{}/",
        tenant_prefix(&ctx.entity).unwrap_or_else(|| "_shared".to_string()),
        ctx.schema.name.as_str(),
        ctx.entity.id
    );
    let key_tail_prefix = format!("/{field}/");
    if !body.key.starts_with(&expected_prefix) || !body.key.contains(&key_tail_prefix) {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "confirm key '{}' does not belong to {}.{}[{}]",
                body.key,
                ctx.schema.name.as_str(),
                field,
                ctx.entity.id
            )],
        });
    }

    let client = resolve_backend(&ctx.registry, &ctx.constraints.bucket)?;
    let head = client
        .head_object(&body.key)
        .await
        .map_err(|e| ForgeError::BackendUnavailable {
            message: format!("HeadObject failed for '{}': {e}", body.key),
        })?
        .ok_or_else(|| ForgeError::ValidationFailed {
            details: vec![format!("no object at key '{}' — upload did not complete", body.key)],
        })?;

    if head.size > ctx.constraints.max_size_bytes {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "uploaded size {} exceeds max_size {} for field '{}'",
                head.size, ctx.constraints.max_size_bytes, field
            )],
        });
    }
    let observed_mime = head.content_type.unwrap_or_default();
    if !observed_mime.is_empty()
        && !ctx.constraints.mime_allowlist.iter().any(|m| m.matches(&observed_mime))
    {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "uploaded content-type '{observed_mime}' not in allowlist for field '{field}'"
            )],
        });
    }

    // If no scan hook is configured for this schema, transition straight to
    // Available. Otherwise leave the attachment in Scanning until an external
    // scanner calls back via the hook (Layer 5 wires that callback endpoint).
    let status = if ctx.schema.hook_for(HookEvent::OnScanComplete).is_some() {
        FileStatus::Scanning
    } else {
        FileStatus::Available
    };

    let attachment = FileAttachment {
        key: body.key,
        size: head.size,
        mime: if observed_mime.is_empty() {
            // Fall back to the constraint's first exact entry (rare: some
            // backends omit Content-Type on HEAD).
            ctx.constraints
                .mime_allowlist
                .first()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string())
        } else {
            observed_mime
        },
        checksum: body.checksum_sha256,
        status,
        created_at: Utc::now(),
        uploaded_at: Some(Utc::now()),
    };

    persist_attachment(&state, &ctx, &field, &attachment).await?;

    // Fire `after_upload` detached — runs under HookDispatchActor supervision.
    // Carries a freshly minted short-TTL presigned GET so the scanner hook can
    // read bytes directly from S3 without streaming through the runtime.
    let download_url = client
        .presign_get(&attachment.key, None)
        .await
        .ok();
    fire_file_after_hook(
        &state,
        &ctx.schema,
        HookEvent::AfterUpload,
        "confirm_upload",
        claims.as_ref(),
        Some(ctx.entity.id.as_str().to_string()),
        file_hook_fields(
            &field,
            "",
            &attachment.mime,
            attachment.size,
            Some(attachment.key.clone()),
            Some(attachment.status.as_str().to_string()),
            download_url,
        ),
    )
    .await;

    Ok(Json(AttachmentResponse {
        status: attachment.status.as_str().to_string(),
        attachment,
    }))
}

// ---------------------------------------------------------------------------
// Scan-complete callback
// ---------------------------------------------------------------------------

/// Request body for `POST /.../scan-complete`.
#[derive(Debug, Deserialize)]
pub struct ScanCompleteRequest {
    /// Terminal status. Must be either `"available"` or `"quarantined"`.
    pub status: String,
    /// Optional reason to surface in audit logs when quarantining.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Transition a file attachment's state after an external scanner reports its
/// verdict. Only `platform_admin` role may invoke (typical deployment runs
/// the scanner service under a `platform_admin` service account).
#[instrument(skip(state, claims, body), fields(schema, entity_id))]
pub async fn scan_complete(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, entity_id, field)): Path<(String, String, String)>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<ScanCompleteRequest>,
) -> Result<Json<AttachmentResponse>, ForgeError> {
    // Only platform_admin identities may move files through terminal states.
    // This is a deliberate simplification: in production deployments the
    // scanner service runs under a platform_admin service account.
    let caller = claims.as_ref().ok_or_else(|| ForgeError::Unauthorized {
        message: "scan-complete requires authenticated platform_admin".into(),
    })?;
    if !caller.has_role(PLATFORM_ADMIN_ROLE) {
        return Err(ForgeError::Forbidden {
            message: "scan-complete requires platform_admin role".into(),
        });
    }

    let new_status = FileStatus::from_str(&body.status).map_err(|()| {
        ForgeError::ValidationFailed {
            details: vec![format!(
                "status must be 'available' or 'quarantined', got '{}'",
                body.status
            )],
        }
    })?;
    if !matches!(new_status, FileStatus::Available | FileStatus::Quarantined) {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "status '{}' is not a terminal scan outcome",
                body.status
            )],
        });
    }

    let ctx = load_file_context(&state, &schema, &entity_id, &field, claims.as_ref()).await?;
    let mut current = current_attachment(&ctx.entity, &field).ok_or_else(|| {
        ForgeError::ValidationFailed {
            details: vec![format!(
                "no attachment present on {}.{}[{}] to scan",
                ctx.schema.name.as_str(),
                field,
                ctx.entity.id
            )],
        }
    })?;

    if current.status != FileStatus::Scanning {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "attachment is in state '{}'; expected 'scanning'",
                current.status.as_str()
            )],
        });
    }

    current.status = new_status;
    persist_attachment(&state, &ctx, &field, &current).await?;

    fire_file_after_hook(
        &state,
        &ctx.schema,
        HookEvent::OnScanComplete,
        "scan_complete",
        claims.as_ref(),
        Some(ctx.entity.id.as_str().to_string()),
        file_hook_fields(
            &field,
            "",
            &current.mime,
            current.size,
            Some(current.key.clone()),
            Some(current.status.as_str().to_string()),
            body.reason
                .as_ref()
                .map(|r| format!("reason:{r}")),
        ),
    )
    .await;

    Ok(Json(AttachmentResponse {
        status: current.status.as_str().to_string(),
        attachment: current,
    }))
}

/// Serve a file field's bytes, either via presigned redirect or streamed proxy.
#[instrument(skip(state, claims))]
pub async fn download_file(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, entity_id, field)): Path<(String, String, String)>,
    Query(query): Query<DownloadQuery>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<Response, ForgeError> {
    let ctx = load_file_context(&state, &schema, &entity_id, &field, claims.as_ref()).await?;
    check_schema_access(&ctx.schema, claims.as_ref(), AccessAction::Read)?;

    let attachment = current_attachment(&ctx.entity, &field).ok_or_else(|| {
        ForgeError::EntityNotFound {
            schema: ctx.schema.name.as_str().to_string(),
            entity_id: format!("{}#{field}", ctx.entity.id),
        }
    })?;

    if attachment.status != FileStatus::Available {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "file not yet available (status: {})",
                attachment.status.as_str()
            )],
        });
    }

    let client = resolve_backend(&ctx.registry, &ctx.constraints.bucket)?;

    match ctx.constraints.access {
        FileAccess::Presigned => {
            let url = client
                .presign_get(&attachment.key, None)
                .await
                .map_err(|e| ForgeError::Internal {
                    message: format!("failed to presign download for '{}': {e}", attachment.key),
                })?;
            if query.redirect.unwrap_or(true) {
                Ok(Redirect::temporary(&url).into_response())
            } else {
                Ok(Json(serde_json::json!({ "url": url, "key": attachment.key })).into_response())
            }
        }
        FileAccess::Proxied => {
            let stream = client
                .get_object_stream(&attachment.key)
                .await
                .map_err(|e| ForgeError::BackendUnavailable {
                    message: format!("GetObject failed for '{}': {e}", attachment.key),
                })?;

            // `ByteStream` implements the smithy `Stream<Item = Result<Bytes, _>>`
            // contract. `futures::stream::unfold` plus `Body::from_stream` adapts
            // it to axum without buffering the whole body.
            let futures_stream = futures::stream::try_unfold(stream.body, |mut bs| async move {
                match bs.next().await {
                    Some(Ok(chunk)) => Ok(Some((chunk, bs))),
                    Some(Err(e)) => Err(std::io::Error::other(e.to_string())),
                    None => Ok(None),
                }
            });
            let body = Body::from_stream(futures_stream);

            let mut headers = HeaderMap::new();
            if let Some(ct) = stream.content_type.as_deref() {
                if let Ok(hv) = HeaderValue::from_str(ct) {
                    headers.insert(header::CONTENT_TYPE, hv);
                }
            } else if let Ok(hv) = HeaderValue::from_str(&attachment.mime) {
                headers.insert(header::CONTENT_TYPE, hv);
            }
            if stream.content_length > 0 {
                if let Ok(hv) = HeaderValue::from_str(&stream.content_length.to_string()) {
                    headers.insert(header::CONTENT_LENGTH, hv);
                }
            }
            Ok((StatusCode::OK, headers, body).into_response())
        }
    }
}

// ---------------------------------------------------------------------------
// Internal plumbing
// ---------------------------------------------------------------------------

struct FileContext {
    schema: SchemaDefinition,
    entity: Entity,
    constraints: FileConstraints,
    registry: StorageRegistry,
}

async fn ask_forge<T>(rx: oneshot::Receiver<T>) -> Result<T, ForgeError> {
    tokio::time::timeout(ACTOR_TIMEOUT, rx)
        .await
        .map_err(|_| ForgeError::Internal {
            message: "forge actor timeout".into(),
        })?
        .map_err(|_| ForgeError::Internal {
            message: "forge actor unavailable".into(),
        })
}

async fn load_file_context(
    state: &AppState<SchemaForgeConfig>,
    schema: &str,
    entity_id: &str,
    field: &str,
    _claims: Option<&Claims>,
) -> Result<FileContext, ForgeError> {
    let schema_name = SchemaName::new(schema).map_err(|_| ForgeError::InvalidSchemaName {
        name: schema.to_string(),
    })?;
    let eid = EntityId::parse(entity_id).map_err(|_| ForgeError::InvalidEntityId {
        id: entity_id.to_string(),
    })?;

    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;

    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or_else(|| ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    let field_def = schema_def.field(field).ok_or_else(|| {
        ForgeError::ValidationFailed {
            details: vec![format!(
                "schema '{}' has no field '{field}'",
                schema_name.as_str()
            )],
        }
    })?;
    let constraints = match &field_def.field_type {
        FieldType::File(c) => c.clone(),
        other => {
            return Err(ForgeError::ValidationFailed {
                details: vec![format!(
                    "field '{field}' on '{}' is of type {other}, not file",
                    schema_name.as_str()
                )],
            });
        }
    };

    let (tx, rx) = oneshot::channel();
    forge
        .send(GetEntity {
            schema: schema_name.clone(),
            id: eid.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let entity = ask_forge(rx)
        .await?
        .map_err(ForgeError::from)?;

    let (tx, rx) = oneshot::channel();
    forge
        .send(GetStorageRegistry {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let registry = ask_forge(rx).await?;

    Ok(FileContext {
        schema: schema_def,
        entity,
        constraints,
        registry,
    })
}

fn resolve_backend(
    registry: &StorageRegistry,
    bucket: &str,
) -> Result<Arc<S3Client>, ForgeError> {
    registry
        .get(bucket)
        .ok_or_else(|| ForgeError::Internal {
            message: format!(
                "storage backend '{bucket}' not configured — add [schema_forge.storage.backends.{bucket}]"
            ),
        })
}

fn validate_upload_request(
    req: &MintUploadUrlRequest,
    constraints: &FileConstraints,
) -> Result<(), ForgeError> {
    if req.size == 0 {
        return Err(ForgeError::ValidationFailed {
            details: vec!["size must be greater than zero".into()],
        });
    }
    if req.size > constraints.max_size_bytes {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "size {} exceeds field max_size {}",
                req.size, constraints.max_size_bytes
            )],
        });
    }
    if req.mime.is_empty() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["mime must not be empty".into()],
        });
    }
    if !constraints.mime_allowlist.iter().any(|m| m.matches(&req.mime)) {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "mime '{}' not in allowlist for this field",
                req.mime
            )],
        });
    }
    if req.filename.trim().is_empty() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["filename must not be empty".into()],
        });
    }
    Ok(())
}

/// Returns the tenant segment for object keys, if the entity is tenant-scoped.
fn tenant_prefix(entity: &Entity) -> Option<String> {
    match entity.fields.get("_tenant") {
        Some(DynamicValue::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

/// Build a deterministic, collision-resistant object key.
fn build_object_key(
    schema: &SchemaName,
    entity_id: &EntityId,
    field: &str,
    filename: &str,
    tenant: Option<String>,
) -> String {
    let tenant_seg = tenant.unwrap_or_else(|| "_shared".to_string());
    let uid = Uuid::now_v7();
    let safe_filename = sanitize_filename(filename);
    format!(
        "{tenant_seg}/{}/{entity_id}/{field}/{uid}/{safe_filename}",
        schema.as_str()
    )
}

/// Replace any character that is not in `[a-zA-Z0-9._-]` with `_` and cap length.
fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    let candidate: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out = if candidate.is_empty() {
        "unnamed".to_string()
    } else {
        candidate
    };
    if out.len() > 255 {
        out[out.len() - 255..].to_string()
    } else {
        out
    }
}

/// Extract the current `FileAttachment` from an entity's field, if any.
///
/// The stored shape is the flat `FileAttachment` JSON object (keys `key`,
/// `size`, `mime`, `checksum`, `status`, `uploaded_at`). When the entity comes
/// back from the backend, the field's `DynamicValue` is a `Composite` or `Json`
/// wrapping that shape. Callers must unwrap the payload directly instead of
/// going through `serde_json::to_value(&DynamicValue)`, whose tagged-enum
/// representation (`#[serde(tag = "type", content = "value")]`) would produce
/// `{"type":"Composite","value":{...}}` and fail to deserialize as
/// `FileAttachment`.
fn current_attachment(entity: &Entity, field: &str) -> Option<FileAttachment> {
    let value = entity.fields.get(field)?;
    let json = dynamic_value_to_attachment_json(value)?;
    serde_json::from_value(json).ok()
}

/// Convert the entity field's `DynamicValue` back to the flat JSON object
/// originally persisted via [`persist_attachment`]. Returns `None` for `Null`
/// or any shape that cannot represent a file attachment object.
fn dynamic_value_to_attachment_json(value: &DynamicValue) -> Option<serde_json::Value> {
    match value {
        DynamicValue::Null => None,
        DynamicValue::Json(v) => Some(v.clone()),
        DynamicValue::Composite(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), dynamic_inner_to_json(v)))
                .collect();
            Some(serde_json::Value::Object(obj))
        }
        _ => None,
    }
}

/// Inner-value conversion used by [`dynamic_value_to_attachment_json`].
///
/// Mirrors the one-way translation in [`json_to_dynamic`]: primitives go
/// straight to their JSON equivalents; nested composites recurse without the
/// tagged-enum wrapping that `serde_json::to_value(&DynamicValue)` would add.
fn dynamic_inner_to_json(value: &DynamicValue) -> serde_json::Value {
    match value {
        DynamicValue::Null => serde_json::Value::Null,
        DynamicValue::Text(s) | DynamicValue::Enum(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Integer(i) => serde_json::Value::Number((*i).into()),
        DynamicValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        DynamicValue::Boolean(b) => serde_json::Value::Bool(*b),
        DynamicValue::DateTime(dt) => serde_json::Value::String(dt.to_rfc3339()),
        DynamicValue::Json(v) => v.clone(),
        DynamicValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(dynamic_inner_to_json).collect())
        }
        DynamicValue::Composite(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), dynamic_inner_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        DynamicValue::Ref(id) => serde_json::Value::String(id.as_str().to_string()),
        DynamicValue::RefArray(ids) => serde_json::Value::Array(
            ids.iter()
                .map(|id| serde_json::Value::String(id.as_str().to_string()))
                .collect(),
        ),
        _ => serde_json::Value::Null,
    }
}

async fn persist_attachment(
    state: &AppState<SchemaForgeConfig>,
    ctx: &FileContext,
    field: &str,
    attachment: &FileAttachment,
) -> Result<(), ForgeError> {
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;

    let mut updated = ctx.entity.clone();
    let json = serde_json::to_value(attachment).map_err(|e| ForgeError::Internal {
        message: format!("attachment serialization failed: {e}"),
    })?;
    let dyn_value = json_to_dynamic(json);
    updated.fields.insert(field.to_string(), dyn_value);

    let (tx, rx) = oneshot::channel();
    forge
        .send(UpdateEntity {
            entity: updated,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let _ = ask_forge(rx).await?.map_err(ForgeError::from)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Hook dispatch helpers
// ---------------------------------------------------------------------------

/// Build the fields map carried by every file hook invocation.
///
/// The runtime's hook dispatcher maps these named keys onto the matching fields
/// in the generated proto message (see `schema-forge-cli/src/commands/hooks.rs`
/// for emission logic); unknown fields are dropped silently.
fn file_hook_fields(
    field_name: &str,
    filename: &str,
    mime: &str,
    size: u64,
    key: Option<String>,
    status: Option<String>,
    download_url: Option<String>,
) -> BTreeMap<String, DynamicValue> {
    let mut fields = BTreeMap::new();
    fields.insert(
        "field_name".to_string(),
        DynamicValue::Text(field_name.to_string()),
    );
    if !filename.is_empty() {
        fields.insert(
            "file_name".to_string(),
            DynamicValue::Text(filename.to_string()),
        );
    }
    fields.insert(
        "mime_type".to_string(),
        DynamicValue::Text(mime.to_string()),
    );
    fields.insert(
        "file_size".to_string(),
        DynamicValue::Integer(i64::try_from(size).unwrap_or(i64::MAX)),
    );
    if let Some(k) = key {
        fields.insert("object_key".to_string(), DynamicValue::Text(k));
    }
    if let Some(s) = status {
        fields.insert("status".to_string(), DynamicValue::Text(s));
    }
    if let Some(url) = download_url {
        fields.insert("download_url".to_string(), DynamicValue::Text(url));
    }
    fields
}

async fn fetch_hook_dispatcher(
    state: &AppState<SchemaForgeConfig>,
) -> Option<Arc<dyn HookDispatcher>> {
    let forge = state.actor::<ForgeActor>()?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetHookDispatcher {
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await.ok().flatten()
}

async fn run_file_before_hook(
    state: &AppState<SchemaForgeConfig>,
    schema: &SchemaDefinition,
    event: HookEvent,
    operation: &str,
    claims: Option<&Claims>,
    entity_id: Option<String>,
    fields: BTreeMap<String, DynamicValue>,
) -> Result<(), ForgeError> {
    if schema.hook_for(event).is_none() {
        return Ok(());
    }
    let hooks_config: HooksConfig = state.config().custom.schema_forge.hooks.clone();
    if !hooks_config.enabled {
        return Ok(());
    }
    let dispatcher = match fetch_hook_dispatcher(state).await {
        Some(d) => d,
        None => return Ok(()),
    };
    let invocation = HookInvocation {
        schema: schema.name.as_str().to_string(),
        event,
        operation: operation.to_string(),
        user_id: claims.map(|c| c.sub.clone()),
        entity_id,
        fields,
    };
    let _ = run_before_hook(dispatcher.as_ref(), &hooks_config, invocation)
        .await
        .map_err(ForgeError::from)?;
    Ok(())
}

async fn fire_file_after_hook(
    state: &AppState<SchemaForgeConfig>,
    schema: &SchemaDefinition,
    event: HookEvent,
    operation: &str,
    claims: Option<&Claims>,
    entity_id: Option<String>,
    fields: BTreeMap<String, DynamicValue>,
) {
    if schema.hook_for(event).is_none() {
        return;
    }
    let hooks_config: HooksConfig = state.config().custom.schema_forge.hooks.clone();
    if !hooks_config.enabled {
        return;
    }
    let dispatcher = match fetch_hook_dispatcher(state).await {
        Some(d) => d,
        None => {
            debug!(?event, "no hook dispatcher available for file after-hook");
            return;
        }
    };
    let invocation = HookInvocation {
        schema: schema.name.as_str().to_string(),
        event,
        operation: operation.to_string(),
        user_id: claims.map(|c| c.sub.clone()),
        entity_id,
        fields,
    };
    match state.actor::<HookDispatchActor>() {
        Some(actor) => {
            actor
                .send(DispatchHook {
                    invocation,
                    dispatcher: Some(dispatcher),
                    config: hooks_config,
                })
                .await;
        }
        None => {
            debug!(?event, "HookDispatchActor not registered; after-hook dropped");
        }
    }
}

fn json_to_dynamic(value: serde_json::Value) -> DynamicValue {
    match value {
        serde_json::Value::Null => DynamicValue::Null,
        serde_json::Value::Bool(b) => DynamicValue::Boolean(b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(DynamicValue::Integer)
            .or_else(|| n.as_f64().map(DynamicValue::Float))
            .unwrap_or(DynamicValue::Null),
        serde_json::Value::String(s) => DynamicValue::Text(s),
        serde_json::Value::Array(arr) => {
            DynamicValue::Array(arr.into_iter().map(json_to_dynamic).collect())
        }
        serde_json::Value::Object(obj) => {
            let mut map = BTreeMap::new();
            for (k, v) in obj {
                map.insert(k, json_to_dynamic(v));
            }
            DynamicValue::Composite(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{EntityId, SchemaName};

    #[test]
    fn sanitize_filename_replaces_unsafe_chars() {
        assert_eq!(sanitize_filename("foo/bar.pdf"), "foo_bar.pdf");
        assert_eq!(sanitize_filename("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_filename("spaces are ok"), "spaces_are_ok");
        assert_eq!(sanitize_filename("unicode-ñ.pdf"), "unicode-_.pdf");
    }

    #[test]
    fn sanitize_filename_preserves_allowed_chars() {
        assert_eq!(
            sanitize_filename("report_2025-01.pdf"),
            "report_2025-01.pdf"
        );
    }

    #[test]
    fn sanitize_filename_fallbacks_to_unnamed_when_blank() {
        assert_eq!(sanitize_filename(""), "unnamed");
        assert_eq!(sanitize_filename("   "), "unnamed");
    }

    #[test]
    fn build_object_key_includes_all_segments() {
        let schema = SchemaName::new("Deal").unwrap();
        let id = EntityId::new("deal");
        let key = build_object_key(&schema, &id, "contract", "proposal.pdf", None);
        assert!(key.starts_with("_shared/Deal/"));
        assert!(key.contains("/contract/"));
        assert!(key.ends_with("/proposal.pdf"));
    }

    #[test]
    fn build_object_key_honors_tenant_segment() {
        let schema = SchemaName::new("Deal").unwrap();
        let id = EntityId::new("deal");
        let key = build_object_key(
            &schema,
            &id,
            "contract",
            "p.pdf",
            Some("tenant_abc".to_string()),
        );
        assert!(key.starts_with("tenant_abc/Deal/"));
    }

    #[test]
    fn current_attachment_round_trips_through_dynamic_value_composite() {
        // Issue #45: the read path returned Composite(map); the old
        // `current_attachment` then called `serde_json::to_value(&DynamicValue)`,
        // which the #[serde(tag, content)] enum turned into
        // `{"type":"Composite","value":{...}}`, breaking `FileAttachment::deserialize`.
        use chrono::Utc;
        use schema_forge_core::types::{FileAttachment, FileStatus, SchemaName};

        let schema = SchemaName::new("Document").unwrap();
        let eid = EntityId::new("document");
        let attachment = FileAttachment {
            key: "tenant/Document/abc/attachment/01HX/report.pdf".into(),
            size: 2_048,
            mime: "application/pdf".into(),
            checksum: Some("sha256:deadbeef".into()),
            status: FileStatus::Available,
            created_at: Utc::now(),
            uploaded_at: Some(Utc::now()),
        };

        let stored_json = serde_json::to_value(&attachment).unwrap();
        let stored_dv = json_to_dynamic(stored_json);

        let mut fields = std::collections::BTreeMap::new();
        fields.insert("attachment".to_string(), stored_dv);
        let entity = Entity::with_id(eid, schema, fields);

        let recovered =
            current_attachment(&entity, "attachment").expect("should recover FileAttachment");
        assert_eq!(recovered.key, attachment.key);
        assert_eq!(recovered.size, attachment.size);
        assert_eq!(recovered.mime, attachment.mime);
        assert_eq!(recovered.checksum, attachment.checksum);
        assert_eq!(recovered.status, attachment.status);
    }

    #[test]
    fn current_attachment_returns_none_for_null() {
        use schema_forge_core::types::SchemaName;

        let schema = SchemaName::new("Document").unwrap();
        let eid = EntityId::new("document");
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("attachment".to_string(), DynamicValue::Null);
        let entity = Entity::with_id(eid, schema, fields);

        assert!(current_attachment(&entity, "attachment").is_none());
    }

    #[test]
    fn current_attachment_works_when_value_is_json_variant() {
        // Belt-and-suspenders: covers a future read-path that returns
        // `DynamicValue::Json(v)` instead of `Composite(map)`.
        use chrono::Utc;
        use schema_forge_core::types::{FileAttachment, FileStatus, SchemaName};

        let schema = SchemaName::new("Document").unwrap();
        let eid = EntityId::new("document");
        let attachment = FileAttachment {
            key: "k".into(),
            size: 10,
            mime: "text/plain".into(),
            checksum: None,
            status: FileStatus::Scanning,
            created_at: Utc::now(),
            uploaded_at: None,
        };
        let stored_json = serde_json::to_value(&attachment).unwrap();

        let mut fields = std::collections::BTreeMap::new();
        fields.insert("attachment".to_string(), DynamicValue::Json(stored_json));
        let entity = Entity::with_id(eid, schema, fields);

        let recovered = current_attachment(&entity, "attachment").expect("from Json variant");
        assert_eq!(recovered.size, 10);
        assert_eq!(recovered.status, FileStatus::Scanning);
    }

    #[test]
    fn validate_upload_request_rejects_zero_size() {
        use schema_forge_core::types::MimePattern;
        let c = FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 1024,
            mime_allowlist: vec![MimePattern::Exact("application/pdf".into())],
            access: FileAccess::Presigned,
        };
        let req = MintUploadUrlRequest {
            filename: "a.pdf".into(),
            mime: "application/pdf".into(),
            size: 0,
        };
        assert!(validate_upload_request(&req, &c).is_err());
    }

    #[test]
    fn validate_upload_request_rejects_too_large() {
        use schema_forge_core::types::MimePattern;
        let c = FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 1024,
            mime_allowlist: vec![MimePattern::Exact("application/pdf".into())],
            access: FileAccess::Presigned,
        };
        let req = MintUploadUrlRequest {
            filename: "a.pdf".into(),
            mime: "application/pdf".into(),
            size: 2048,
        };
        assert!(validate_upload_request(&req, &c).is_err());
    }

    #[test]
    fn validate_upload_request_rejects_bad_mime() {
        use schema_forge_core::types::MimePattern;
        let c = FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 1024,
            mime_allowlist: vec![MimePattern::Exact("application/pdf".into())],
            access: FileAccess::Presigned,
        };
        let req = MintUploadUrlRequest {
            filename: "a.pdf".into(),
            mime: "image/png".into(),
            size: 512,
        };
        assert!(validate_upload_request(&req, &c).is_err());
    }

    #[test]
    fn validate_upload_request_accepts_wildcard_family() {
        use schema_forge_core::types::MimePattern;
        let c = FileConstraints {
            bucket: "media".into(),
            max_size_bytes: 1024 * 1024,
            mime_allowlist: vec![MimePattern::Family("image".into())],
            access: FileAccess::Presigned,
        };
        let req = MintUploadUrlRequest {
            filename: "hero.png".into(),
            mime: "image/png".into(),
            size: 1024,
        };
        assert!(validate_upload_request(&req, &c).is_ok());
    }

    #[test]
    fn resolve_backend_missing_returns_internal_error() {
        let registry = StorageRegistry::default();
        let err = resolve_backend(&registry, "documents").unwrap_err();
        assert!(matches!(err, ForgeError::Internal { .. }));
    }
}
