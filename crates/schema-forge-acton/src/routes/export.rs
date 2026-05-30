//! Bulk-export endpoint: `POST /schemas/{schema}/entities/export`.
//!
//! An export is **a query with no page limit, materialized to a file**, gated by
//! the two fail-closed controls from ADR-0003 layered on top of the unchanged
//! query path:
//!
//! 1. a distinct Cedar action ([`AccessAction::Export`]) — a policy can permit
//!    read-one while forbidding export-many; and
//! 2. a per-field `@exportable` opt-in — the exported column set is exactly the
//!    intersection of `@exportable` fields and the fields the caller may read.
//!
//! CSV and NDJSON exports whose resolved row count is within the schema's
//! `@export(max_rows)` cap are serialized inline in the response (the
//! **synchronous, streamable** path). Anything else — a non-streamable format
//! (XLSX or ZIP), `async: true`, or a result that would exceed the cap — takes
//! the supervised **async-job** path: a job is registered with the
//! [`ExportJobActor`](crate::export_job::ExportJobActor) and its id is returned
//! immediately. XLSX is async-only because `rust_xlsxwriter` buffers the whole
//! workbook; ZIP is async-only because the multi-member archive (and any bundled
//! file blobs) is staged and buffered before upload.
//!
//! Tenant injection ([`inject_tenant_scope`]) and per-field read stripping
//! ([`filter_entity_fields`]) run identically to the normal query path, so the
//! file can never contain a row or a field the caller could not already read one
//! record at a time. Every request emits the AU-2 exfiltration audit trail
//! (`forge.export.initiated` / `.completed` / `.denied`).

use std::collections::HashMap;
use std::time::Duration;

use acton_service::audit::AuditSeverity;
use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use schema_forge_backend::entity::Entity;
use schema_forge_core::export::{
    to_cell, to_ndjson, to_xlsx_cell, CellOptions, RelationDisplay, XlsxCell,
};
use schema_forge_core::query::{validate_filter, Filter, Query};
use schema_forge_core::types::{
    DynamicValue, EntityId, ExportFormat, FieldType, SchemaDefinition, SchemaName,
};
use serde::Deserialize;
use tokio::sync::oneshot;
use tracing::instrument;

use crate::access::{
    check_export_access, filter_entity_fields, inject_tenant_scope, FieldFilterDirection,
    OptionalClaims,
};
use crate::actor::ForgeActor;
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;
use crate::messages::{
    GetRecordAccessPolicy, GetSchema, GetTenantConfig, QueryEntities, ReplyChannel,
};

/// Timeout for actor request-response round-trips, matching the entities path.
const ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

/// Request body for `POST /schemas/{schema}/entities/export`.
///
/// Mirrors the query body's filter shape so an export is literally "the same
/// query, no page limit". `fields` narrows the requested columns; the server
/// always intersects it with `@exportable` ∩ field-access-readable, so it can
/// never widen what leaves.
#[derive(Debug, Deserialize)]
pub struct ExportRequestBody {
    /// Raw JSON filter — converted to a [`Filter`] using schema type hints,
    /// identical to the query endpoint.
    #[serde(default)]
    pub filter: Option<serde_json::Value>,
    /// Optional caller-requested column subset. Intersected with the
    /// `@exportable` ∩ readable set; never widens it.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    /// Deliverable format, as the DSL keyword (`csv`, `ndjson`, `xlsx`, `zip`).
    /// Parsed to an [`ExportFormat`] via [`ExportRequestBody::parse_format`].
    /// Only `csv` and `ndjson` stream inline; `xlsx` and `zip` take the async-job
    /// path.
    pub format: String,
    /// When `true`, the caller explicitly asks for the async-job path, so even a
    /// streamable format is materialized to storage and returned as a job id
    /// rather than streamed inline.
    #[serde(default, rename = "async")]
    pub is_async: bool,
}

impl ExportRequestBody {
    /// Parse the caller's `format` keyword into an [`ExportFormat`], rejecting
    /// any string outside the canonical vocabulary with a 400.
    pub fn parse_format(&self) -> Result<ExportFormat, ForgeError> {
        self.format
            .parse::<ExportFormat>()
            .map_err(|()| ForgeError::InvalidQuery {
                message: format!(
                    "unknown export format '{}'; expected one of csv, ndjson, xlsx, zip",
                    self.format
                ),
            })
    }
}

/// Whether `format` is one the synchronous path can stream inline.
///
/// CSV and NDJSON are byte-streamable row-by-row; XLSX must be buffered
/// (`rust_xlsxwriter` cannot truly stream) and ZIP needs staging, so both ride
/// the async-job path (ADR-0003).
pub fn is_streamable(format: ExportFormat) -> bool {
    matches!(format, ExportFormat::Csv | ExportFormat::Ndjson)
}

/// Whether the supervised async-job pipeline can currently materialize `format`.
///
/// All declared formats are supported on the async path: CSV and NDJSON (which
/// also stream synchronously), XLSX (async-only because `rust_xlsxwriter` buffers
/// the whole workbook), and ZIP (the multi-member bundle, also buffered). XLSX
/// and ZIP never stream inline (ADR-0003).
pub fn async_job_supports(format: ExportFormat) -> bool {
    matches!(
        format,
        ExportFormat::Csv | ExportFormat::Ndjson | ExportFormat::Xlsx | ExportFormat::Zip
    )
}

/// Compute the ordered export column set: every `@exportable` field on the
/// schema, optionally narrowed to the caller-requested `fields`.
///
/// This is the *static* (schema-level) half of the intersection from ADR-0003.
/// The *dynamic* (per-caller read access) half is applied per row via
/// [`filter_entity_fields`]: a column listed here that a given row strips for
/// read access renders as an empty cell / absent key, never a leak. Returns the
/// field name paired with its `@exportable(flatten: ...)` hint, in schema
/// declaration order so the CSV header is stable.
///
/// `requested` is the caller's optional `fields` subset; a requested field that
/// is not `@exportable` is silently dropped (it can only ever narrow), and an
/// unknown field name is likewise ignored here — column resolution never widens.
pub fn resolve_export_columns(
    schema: &SchemaDefinition,
    requested: Option<&[String]>,
) -> Vec<(String, Option<schema_forge_core::types::ExportFlatten>)> {
    schema
        .fields
        .iter()
        .filter_map(|f| {
            let flatten = f.exportable()?;
            let name = f.name.as_str();
            match requested {
                Some(req) if !req.iter().any(|r| r == name) => None,
                _ => Some((name.to_string(), flatten)),
            }
        })
        .collect()
}

/// A [`RelationDisplay`] backed by a single field's resolved `id -> display`
/// map, so the pure serialization core can resolve relation display values
/// without knowing about the per-field display map shape.
struct FieldDisplay<'a> {
    id_to_display: Option<&'a HashMap<String, String>>,
}

impl RelationDisplay for FieldDisplay<'_> {
    fn display_for(&self, id: &EntityId) -> Option<String> {
        self.id_to_display
            .and_then(|m| m.get(id.as_str()).cloned())
    }
}

/// Serialize already-read, already-field-filtered `entities` into a CSV body
/// (header row + one record per entity) under the export flatten policy.
///
/// `columns` is the resolved `@exportable` ∩ requested column set (see
/// [`resolve_export_columns`]); `display_map` carries per-field resolved
/// relation displays (`field -> id -> display`). A column missing from a row
/// (because field-access stripped it, or the row simply lacks it) renders as an
/// empty cell — fail-closed, never a leak.
pub fn entities_to_csv(
    entities: &[Entity],
    columns: &[(String, Option<schema_forge_core::types::ExportFlatten>)],
    display_map: &HashMap<String, HashMap<String, String>>,
) -> Result<String, String> {
    let header: Vec<&str> = columns.iter().map(|(name, _)| name.as_str()).collect();
    let mut out = schema_forge_core::export::csv_record(&header).map_err(|e| e.to_string())?;

    for entity in entities {
        let mut cells: Vec<String> = Vec::with_capacity(columns.len());
        for (name, flatten) in columns {
            let cell = match entity.field(name) {
                Some(value) => {
                    let displays = FieldDisplay {
                        id_to_display: display_map.get(name),
                    };
                    let opts = CellOptions::new().with_flatten(*flatten);
                    to_cell(value, &opts, &displays)
                }
                None => String::new(),
            };
            cells.push(cell);
        }
        out.push_str(&schema_forge_core::export::csv_record(&cells).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Serialize already-read, already-field-filtered `entities` into an NDJSON body
/// (one JSON object per line) under the lossless export policy.
///
/// Only the resolved `@exportable` ∩ requested columns are emitted; the row's
/// `id` is always included as `"id"`. A column stripped for read access on a
/// given row is simply absent from that line — fail-closed.
pub fn entities_to_ndjson(
    entities: &[Entity],
    columns: &[(String, Option<schema_forge_core::types::ExportFlatten>)],
    display_map: &HashMap<String, HashMap<String, String>>,
) -> String {
    let mut out = String::new();
    for entity in entities {
        let mut obj = serde_json::Map::with_capacity(columns.len() + 1);
        obj.insert(
            "id".to_string(),
            serde_json::Value::String(entity.id.to_string()),
        );
        for (name, _) in columns {
            if let Some(value) = entity.field(name) {
                let displays = FieldDisplay {
                    id_to_display: display_map.get(name),
                };
                obj.insert(name.clone(), to_ndjson(value, &displays));
            }
        }
        // serde_json never fails to serialize an in-memory object.
        if let Ok(line) = serde_json::to_string(&serde_json::Value::Object(obj)) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Number format applied to native datetime cells so Excel renders a UTC
/// timestamp as a readable date-time rather than a raw serial number.
const XLSX_DATETIME_FORMAT: &str = "yyyy-mm-dd hh:mm:ss";

/// Serialize already-read, already-field-filtered `entities` into an XLSX
/// workbook (a header row plus one row per entity) under the export flatten
/// policy, returning the `.xlsx` file bytes.
///
/// XLSX is a rectangular projection like CSV, but cells keep their native
/// spreadsheet *type* where it helps: numbers, booleans, and datetimes are
/// written as native cell types (so they sort, sum, and format), while
/// structured values (relation-many / array / map / composite / json) are
/// JSON-encoded into a text cell exactly as the CSV path does. The pure
/// [`to_xlsx_cell`] core decides the cell type; this function only translates
/// each [`XlsxCell`] into a `rust_xlsxwriter` write. `rust_xlsxwriter` buffers
/// the whole workbook, which is why XLSX rides the async/buffered path
/// (ADR-0003) and never streams.
///
/// `columns` is the resolved `@exportable` ∩ requested column set; a column
/// missing from a row (stripped for read access, or simply absent) renders as an
/// empty cell — fail-closed, never a leak.
pub fn entities_to_xlsx(
    entities: &[Entity],
    columns: &[(String, Option<schema_forge_core::types::ExportFlatten>)],
    display_map: &HashMap<String, HashMap<String, String>>,
) -> Result<Vec<u8>, String> {
    use rust_xlsxwriter::{Format, Workbook};

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    let datetime_format = Format::new().set_num_format(XLSX_DATETIME_FORMAT);

    // Header row (row 0): the exportable column names, in declaration order.
    for (col_idx, (name, _)) in columns.iter().enumerate() {
        let col = u16::try_from(col_idx).map_err(|_| "too many export columns".to_string())?;
        worksheet
            .write_string(0, col, name.as_str())
            .map_err(|e| e.to_string())?;
    }

    // Data rows start at row 1.
    for (row_offset, entity) in entities.iter().enumerate() {
        let row = u32::try_from(row_offset + 1).map_err(|_| "too many export rows".to_string())?;
        for (col_idx, (name, flatten)) in columns.iter().enumerate() {
            let col = u16::try_from(col_idx).map_err(|_| "too many export columns".to_string())?;
            let cell = match entity.field(name) {
                Some(value) => {
                    let displays = FieldDisplay {
                        id_to_display: display_map.get(name),
                    };
                    let opts = CellOptions::new().with_flatten(*flatten);
                    to_xlsx_cell(value, &opts, &displays)
                }
                None => XlsxCell::Empty,
            };
            write_xlsx_cell(worksheet, row, col, cell, &datetime_format)
                .map_err(|e| e.to_string())?;
        }
    }

    workbook.save_to_buffer().map_err(|e| e.to_string())
}

/// Translate one typed [`XlsxCell`] into a `rust_xlsxwriter` write at `(row, col)`.
///
/// An [`XlsxCell::Empty`] writes nothing (a blank cell), keeping the fail-closed
/// "absent field renders empty" contract. Datetimes carry `datetime_format` so
/// Excel shows a timestamp rather than a serial number.
fn write_xlsx_cell(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    cell: XlsxCell,
    datetime_format: &rust_xlsxwriter::Format,
) -> Result<(), rust_xlsxwriter::XlsxError> {
    match cell {
        XlsxCell::Empty => Ok(()),
        XlsxCell::Text(s) => worksheet.write_string(row, col, &s).map(|_| ()),
        XlsxCell::Number(n) => worksheet.write_number(row, col, n).map(|_| ()),
        XlsxCell::Boolean(b) => worksheet.write_boolean(row, col, b).map(|_| ()),
        // rust_xlsxwriter's chrono feature accepts a tz-naive `NaiveDateTime`;
        // the value is already UTC, so dropping the (UTC) offset is lossless and
        // the cell carries the timestamp Excel renders via XLSX_DATETIME_FORMAT.
        XlsxCell::DateTime(dt) => worksheet
            .write_datetime_with_format(row, col, dt.naive_utc(), datetime_format)
            .map(|_| ()),
    }
}

/// A fully materialized export: the serialized bytes plus the metadata both the
/// inline-stream response and the async-job completion record need.
///
/// Produced by [`materialize_export`], which runs the identical query / tenant /
/// field-strip pipeline as the synchronous path so the async artifact can never
/// contain a row or field the caller could not read.
pub struct ExportArtifact {
    /// Serialized file bytes (CSV or NDJSON).
    pub bytes: Vec<u8>,
    /// MIME content type for the response / stored object.
    pub content_type: &'static str,
    /// Number of rows that survived record-level access filtering.
    pub row_count: usize,
    /// The resolved `@exportable` ∩ readable column names, for the audit trail.
    pub column_names: Vec<String>,
}

/// The authorization-resolved inputs the export pipeline needs, shared by the
/// synchronous handler and the async-job actor.
///
/// Bundling these keeps [`materialize_export`] to a small argument list and lets
/// the actor carry exactly what the handler resolved (tenant scope, the compiled
/// policy store, and the record-access policy) so generation applies the **same**
/// authz the query path enforces.
pub struct ExportContext<'a> {
    /// Handle to the `ForgeActor` for running the query / display lookups.
    pub forge: &'a acton_service::prelude::ActorHandle,
    /// Caller claims, used for tenant scoping and record-level filtering.
    pub claims: Option<&'a Claims>,
    /// Resolved tenant configuration (injected into the query).
    pub tenant_config: &'a Option<schema_forge_backend::tenant::TenantConfig>,
    /// Compiled Cedar policy store driving per-field read stripping.
    pub policy_store: &'a std::sync::Arc<crate::authz::PolicyStore>,
    /// Record-level access policy (e.g. `@owner`) applied before serialization.
    pub record_access_policy:
        &'a Option<std::sync::Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>>,
}

/// Run the full export pipeline — query (no page limit, `max_rows`-capped),
/// tenant injection, record-level access filter, relation-display resolution,
/// per-field read stripping, and serialization — and return the materialized
/// artifact.
///
/// This is the single source of truth shared by the synchronous streaming
/// handler and the async export-job actor, so both paths inherit the exact same
/// fail-closed guarantees. `max_rows` is enforced by probing `max_rows + 1` and
/// returning [`ForgeError::ExportTooLarge`] on overflow.
pub async fn materialize_export(
    ctx: &ExportContext<'_>,
    schema_def: &SchemaDefinition,
    format: ExportFormat,
    filter_json: Option<&serde_json::Value>,
    requested_fields: Option<&[String]>,
    max_rows: u64,
) -> Result<ExportArtifact, ForgeError> {
    let prepared = prepare_export(ctx, schema_def, filter_json, requested_fields, max_rows).await?;
    let PreparedExport {
        entities,
        columns,
        display_map,
    } = &prepared;

    let row_count = entities.len();
    let (bytes, content_type) = match format {
        ExportFormat::Csv => {
            let csv = entities_to_csv(entities, columns, display_map).map_err(|e| {
                ForgeError::Internal {
                    message: format!("CSV serialization failed: {e}"),
                }
            })?;
            (csv.into_bytes(), "text/csv; charset=utf-8")
        }
        ExportFormat::Ndjson => {
            let ndjson = entities_to_ndjson(entities, columns, display_map);
            (ndjson.into_bytes(), "application/x-ndjson")
        }
        ExportFormat::Xlsx => {
            let xlsx = entities_to_xlsx(entities, columns, display_map).map_err(|e| {
                ForgeError::Internal {
                    message: format!("XLSX serialization failed: {e}"),
                }
            })?;
            (
                xlsx,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            )
        }
        other => {
            return Err(ForgeError::Internal {
                message: format!("unsupported format reached the export pipeline: {other}"),
            });
        }
    };

    Ok(ExportArtifact {
        bytes,
        content_type,
        row_count,
        column_names: prepared.column_names(),
    })
}

/// The result of running the shared query / tenant / record-filter /
/// display-resolve / field-strip half of the export pipeline, before any
/// format-specific serialization.
///
/// Both the flat formats ([`materialize_export`]) and the ZIP bundle
/// ([`materialize_zip_bundle`]) start from the same `PreparedExport`, so a bundle
/// can never contain a row or field a flat export could not — the two paths share
/// the identical fail-closed preparation rather than reimplementing it.
pub struct PreparedExport {
    /// Rows that survived record-level access filtering, each already stripped of
    /// read-restricted fields.
    pub entities: Vec<Entity>,
    /// Resolved `@exportable` ∩ requested column set (name + flatten hint).
    pub columns: Vec<(String, Option<schema_forge_core::types::ExportFlatten>)>,
    /// Per-field resolved relation displays (`field -> id -> display`).
    pub display_map: HashMap<String, HashMap<String, String>>,
}

impl PreparedExport {
    /// The resolved exportable column names, for the audit trail.
    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|(n, _)| n.clone()).collect()
    }
}

/// Run the shared, format-independent half of the export pipeline: build the
/// query (no page limit, `max_rows`-capped), inject the tenant scope, execute,
/// enforce the row cap, apply record-level access filtering, resolve relation
/// displays, and strip read-restricted fields per row.
///
/// Factored out of [`materialize_export`] so the flat serializers and the ZIP
/// bundle path share the **same** tenant injection and field stripping the query
/// path enforces. `max_rows` is enforced by probing `max_rows + 1` and returning
/// [`ForgeError::ExportTooLarge`] on overflow.
pub async fn prepare_export(
    ctx: &ExportContext<'_>,
    schema_def: &SchemaDefinition,
    filter_json: Option<&serde_json::Value>,
    requested_fields: Option<&[String]>,
    max_rows: u64,
) -> Result<PreparedExport, ForgeError> {
    let ExportContext {
        forge,
        claims,
        tenant_config,
        policy_store,
        record_access_policy,
    } = ctx;
    let claims = *claims;

    // Build the query (an export is a query with no page limit). Cap the read at
    // max_rows + 1 so an over-cap result is detectable without draining the table.
    let mut query = Query::new(schema_def.id.clone()).without_total_count();
    if let Some(filter_json) = filter_json {
        let filter = crate::routes::entities::json_to_filter(filter_json, schema_def)
            .map_err(|errors| ForgeError::InvalidQuery {
                message: errors.join("; "),
            })?;
        validate_filter(&filter, schema_def).map_err(|errors| ForgeError::InvalidQuery {
            message: errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        })?;
        query = query.with_filter(filter);
    }
    let probe_limit = usize::try_from(max_rows)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    query = query.with_limit(probe_limit);

    // Same tenant injection as the query path.
    inject_tenant_scope(&mut query, claims, tenant_config);

    // Execute.
    let (tx, rx) = oneshot::channel();
    forge
        .send(QueryEntities {
            query,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let result = ask_forge(rx).await?.map_err(ForgeError::from)?;

    // Over-cap: a result strictly larger than max_rows cannot be exported.
    if result.entities.len() as u64 > max_rows {
        return Err(ForgeError::ExportTooLarge {
            max_rows,
            message: format!(
                "export exceeds the {max_rows}-row cap for schema '{}'",
                schema_def.name.as_str()
            ),
        });
    }

    // Record-level access filtering (e.g. @owner), identical to the query path.
    let mut visible: Vec<Entity> =
        if let (Some(policy), Some(c)) = (record_access_policy.as_ref(), claims) {
            policy.filter_visible(schema_def, c, result.entities).await
        } else {
            result.entities
        };

    // Resolve the export column set (static half of the intersection).
    let columns = resolve_export_columns(schema_def, requested_fields);

    // Resolve relation displays, then strip read-restricted fields per row
    // (dynamic half of the intersection).
    let display_map =
        resolve_export_displays(forge, schema_def, &visible, &columns, claims, tenant_config)
            .await?;
    for entity in &mut visible {
        filter_entity_fields(
            policy_store,
            entity,
            schema_def,
            claims,
            FieldFilterDirection::Read,
        );
    }

    Ok(PreparedExport {
        entities: visible,
        columns,
        display_map,
    })
}

/// The data-file format carried inside a ZIP bundle. CSV is the bundle's
/// rectangular projection (the same one the flat CSV export produces), so the
/// archive's data member is `<schema>.csv`.
const ZIP_DATA_FORMAT: ExportFormat = ExportFormat::Csv;

/// Materialize a ZIP-bundle export and return its bytes.
///
/// The archive always carries the rectangular data file (`<schema>.csv`,
/// generated from the same `@exportable` ∩ readable projection the flat CSV
/// export uses). When the entity's `@export(bundle_files: true)` opt-in is set,
/// the raw blobs of its `@exportable` `file` fields are pulled from `store` and
/// embedded under `files/<field>/<id>/<filename>`. Bundling file blobs is a
/// strictly larger exfiltration surface, so it is reachable only behind that
/// opt-in (the caller passes the schema-declared flag) and the same Export authz
/// the rest of the path enforces; only `Available` (scanned-clean) blobs are
/// embedded.
///
/// `store` is the [`ExportArtifactStore`] seam, so the blob fetch is mockable in
/// tests without a live object store. A blob that cannot be read from storage
/// fails the whole job rather than silently shipping an incomplete bundle.
pub async fn materialize_zip_bundle(
    ctx: &ExportContext<'_>,
    schema_def: &SchemaDefinition,
    filter_json: Option<&serde_json::Value>,
    requested_fields: Option<&[String]>,
    max_rows: u64,
    bundle_files: bool,
    store: &std::sync::Arc<dyn crate::storage::ExportArtifactStore>,
) -> Result<ExportArtifact, ForgeError> {
    use crate::export_bundle::{
        build_zip_bundle, collect_file_blob_refs, data_member_name, BundleEntry,
    };

    let prepared = prepare_export(ctx, schema_def, filter_json, requested_fields, max_rows).await?;
    let row_count = prepared.entities.len();

    // The rectangular data member, identical to the flat CSV projection.
    let csv = entities_to_csv(&prepared.entities, &prepared.columns, &prepared.display_map)
        .map_err(|e| ForgeError::Internal {
            message: format!("CSV serialization failed: {e}"),
        })?;
    let mut entries = vec![BundleEntry::new(
        data_member_name(schema_def.name.as_str(), ZIP_DATA_FORMAT.as_str()),
        csv.into_bytes(),
    )];

    // File-field blobs: only when the entity opted in via `bundle_files: true`.
    // The refs are derived purely from the already-stripped rows, so a blob whose
    // field was not @exportable or not readable by the caller is never referenced.
    if bundle_files {
        let blob_refs = collect_file_blob_refs(&prepared.entities, schema_def, &prepared.columns);
        for blob in blob_refs {
            let bytes = store.get(&blob.key).await.map_err(|e| ForgeError::Internal {
                message: format!("failed to read file blob '{}' for bundle: {e}", blob.key),
            })?;
            entries.push(BundleEntry::new(blob.member_name, bytes));
        }
    }

    let bytes = build_zip_bundle(&entries).map_err(|e| ForgeError::Internal {
        message: format!("ZIP bundle assembly failed: {e}"),
    })?;

    Ok(ExportArtifact {
        bytes,
        content_type: "application/zip",
        row_count,
        column_names: prepared.column_names(),
    })
}

/// Await an actor response with a timeout, mapping failures to `Internal`.
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

/// Enforce the per-subject bulk-export rate limit via the supervised
/// [`ExportRateLimiter`](crate::export_rate_limit::ExportRateLimiter) actor.
///
/// Returns `Ok(())` when the request is admitted and
/// [`ForgeError::RateLimited`] when the subject has exhausted its allowance for
/// the current window. The limiter is keyed by subject (anonymous callers share
/// one coarse bucket). The actor is registered on the serve path alongside the
/// `ExportJobActor`; if it is somehow absent the request is refused fail-closed
/// rather than silently bypassing the limit.
async fn enforce_export_rate_limit(
    state: &AppState<SchemaForgeConfig>,
    settings: &crate::export_config::ExportSettings,
    claims: Option<&Claims>,
) -> Result<(), ForgeError> {
    use crate::export_rate_limit::{rate_limit_key, AdmitExport, ExportRateLimiter};

    let limiter = state
        .actor::<ExportRateLimiter>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ExportRateLimiter not registered".into(),
        })?;

    let key = rate_limit_key(claims.map(|c| c.sub.as_str()));
    let window_ms = settings
        .rate_limit
        .window_secs
        .saturating_mul(1000)
        .max(1);

    let (tx, rx) = oneshot::channel();
    limiter
        .send(AdmitExport {
            key,
            max_requests: settings.rate_limit.max_requests,
            window_ms,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let admitted = ask_forge(rx).await?;

    if admitted {
        Ok(())
    } else {
        Err(ForgeError::RateLimited {
            message: format!(
                "bulk export rate limit exceeded: at most {} export(s) per {}s",
                settings.rate_limit.max_requests, settings.rate_limit.window_secs
            ),
        })
    }
}

/// Emit one of the `forge.export.*` AU-2 audit events.
async fn audit_export(
    state: &AppState<SchemaForgeConfig>,
    event: &str,
    severity: AuditSeverity,
    metadata: serde_json::Value,
) {
    if let Some(logger) = state.audit_logger() {
        logger.log_custom(event, severity, Some(metadata)).await;
    }
}

/// `POST /schemas/{schema}/entities/export` — synchronous streamable export.
///
/// Streams a CSV or NDJSON file inline when the format is streamable and the
/// resolved row count is within the schema's `@export(max_rows)` cap. Rejects
/// (pointing at the async-job endpoint) for non-streamable formats, an explicit
/// `async: true`, or an over-cap result. Fail-closed throughout: a schema with
/// no `@export` cannot be exported, and the column set is the intersection of
/// `@exportable` and what the caller may read.
#[instrument(skip_all, fields(schema = %schema))]
pub async fn export_entities(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<ExportRequestBody>,
) -> Result<Response, ForgeError> {
    let schema_name = SchemaName::new(&schema).map_err(|_| ForgeError::InvalidSchemaName {
        name: schema.clone(),
    })?;
    let format = body.parse_format()?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Resolve the schema definition.
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    // Level-1 fail-closed gate: the entity must declare `@export`, and the
    // requested format must be in its declared formats vocabulary.
    let Some((formats, bundle_files, max_rows)) = schema_def.export_config() else {
        audit_export(
            &state,
            "forge.export.denied",
            AuditSeverity::Warning,
            serde_json::json!({
                "schema": &schema,
                "reason": "export_not_enabled",
                "user": claims.as_ref().map(|c| &c.sub),
            }),
        )
        .await;
        return Err(ForgeError::Forbidden {
            message: format!(
                "schema '{}' is not exportable: no @export annotation",
                schema_name.as_str()
            ),
        });
    };
    if !formats.contains(&format) {
        audit_export(
            &state,
            "forge.export.denied",
            AuditSeverity::Warning,
            serde_json::json!({
                "schema": &schema,
                "reason": "format_not_allowed",
                "format": format.as_str(),
                "user": claims.as_ref().map(|c| &c.sub),
            }),
        )
        .await;
        return Err(ForgeError::Forbidden {
            message: format!(
                "format '{}' is not enabled for schema '{}'",
                format.as_str(),
                schema_name.as_str()
            ),
        });
    }

    // Distinct Cedar `Export{Schema}` authorization — NOT a Read alias.
    let policy_store = crate::routes::entities::fetch_export_policy_store(&state).await?;
    if let Err(e) = check_export_access(&policy_store, &schema_def, claims.as_ref()) {
        audit_export(
            &state,
            "forge.export.denied",
            AuditSeverity::Warning,
            serde_json::json!({
                "schema": &schema,
                "reason": "authz_denied",
                "user": claims.as_ref().map(|c| &c.sub),
            }),
        )
        .await;
        return Err(e);
    }

    // An explicit, non-empty `fields` request that resolves to no `@exportable`
    // column is a denied path: the caller asked only for fields that may never
    // leave in a bulk file. Audit and refuse rather than silently streaming an
    // id-only file (which would mask the attempt). Narrowing a partially-valid
    // request is still allowed — only a wholly-non-exportable request is denied.
    if let Some(requested) = body.fields.as_deref() {
        if !requested.is_empty() && resolve_export_columns(&schema_def, Some(requested)).is_empty() {
            audit_export(
                &state,
                "forge.export.denied",
                AuditSeverity::Warning,
                serde_json::json!({
                    "schema": &schema,
                    "reason": "no_exportable_fields_requested",
                    "format": format.as_str(),
                    "fields": &body.fields,
                    "user": claims.as_ref().map(|c| &c.sub),
                }),
            )
            .await;
            return Err(ForgeError::Forbidden {
                message: format!(
                    "none of the requested fields are exportable for schema '{}'",
                    schema_name.as_str()
                ),
            });
        }
    }

    // Per-subject bulk-export rate limit. Export is an exfiltration surface, so a
    // caller cannot drain a table by issuing many small exports back to back even
    // when each stays under the row cap. Enforced after authz so a denied caller
    // never consumes a rate-limit slot.
    let export_settings = &state.config().custom.schema_forge.export;
    if let Err(e) = enforce_export_rate_limit(&state, export_settings, claims.as_ref()).await {
        audit_export(
            &state,
            "forge.export.denied",
            AuditSeverity::Warning,
            serde_json::json!({
                "schema": &schema,
                "reason": "rate_limited",
                "format": format.as_str(),
                "user": claims.as_ref().map(|c| &c.sub),
            }),
        )
        .await;
        return Err(e);
    }

    // The effective row cap is the schema's `@export(max_rows)` intersected with
    // the server-wide ceiling: a schema may declare a tighter cap, never one
    // above what the operator permits (fail-closed).
    let max_rows = crate::export_config::resolve_max_rows(max_rows, export_settings.default_max_rows);

    // Initiated: the request passed the entity-level and authz gates. Record
    // the requested filter/fields/format for the exfiltration trail.
    audit_export(
        &state,
        "forge.export.initiated",
        AuditSeverity::Informational,
        serde_json::json!({
            "schema": &schema,
            "format": format.as_str(),
            "filter": &body.filter,
            "fields": &body.fields,
            "user": claims.as_ref().map(|c| &c.sub),
        }),
    )
    .await;

    // Resolve the tenant config + record-access policy once; both the sync and
    // async paths need them, and they are the same inputs the query path uses.
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetTenantConfig {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let tenant_config = ask_forge(rx).await?;

    let (tx, rx) = oneshot::channel();
    forge
        .send(GetRecordAccessPolicy {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let record_access_policy = ask_forge(rx).await?;

    // Non-streamable format or explicit async => take the supervised async-job
    // path: register a job with the ExportJobActor and return its id immediately.
    if body.is_async || !is_streamable(format) {
        let authz = ResolvedExportAuthz {
            tenant_config,
            policy_store,
            record_access_policy,
        };
        let cfg = ExportJobConfig {
            format,
            max_rows,
            bundle_files,
        };
        return spawn_export_job(&state, &schema_def, cfg, &body, claims.as_ref(), authz).await;
    }

    // Synchronous streamable path: materialize inline and stream the bytes.
    let ctx = ExportContext {
        forge,
        claims: claims.as_ref(),
        tenant_config: &tenant_config,
        policy_store: &policy_store,
        record_access_policy: &record_access_policy,
    };
    let artifact = match materialize_export(
        &ctx,
        &schema_def,
        format,
        body.filter.as_ref(),
        body.fields.as_deref(),
        max_rows,
    )
    .await
    {
        Ok(a) => a,
        Err(ForgeError::ExportTooLarge { max_rows, message }) => {
            audit_export(
                &state,
                "forge.export.denied",
                AuditSeverity::Warning,
                serde_json::json!({
                    "schema": &schema,
                    "reason": "row_cap_exceeded",
                    "max_rows": max_rows,
                    "format": format.as_str(),
                    "user": claims.as_ref().map(|c| &c.sub),
                }),
            )
            .await;
            return Err(ForgeError::ExportTooLarge {
                max_rows,
                message: format!(
                    "{message}; use the async job endpoint (POST with async:true)"
                ),
            });
        }
        Err(e) => return Err(e),
    };

    audit_export(
        &state,
        "forge.export.completed",
        AuditSeverity::Informational,
        serde_json::json!({
            "schema": &schema,
            "format": format.as_str(),
            "filter": &body.filter,
            "fields": &artifact.column_names,
            "row_count": artifact.row_count,
            "user": claims.as_ref().map(|c| &c.sub),
        }),
    )
    .await;

    let filename = format!("{}.{}", schema_name.as_str(), format.as_str());
    let response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, artifact.content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        artifact.bytes,
    )
        .into_response();
    Ok(response)
}

/// Owned authorization-resolved inputs handed to the async export job.
///
/// The owned counterpart of [`ExportContext`]: the job actor outlives the
/// request, so it carries the tenant scope, compiled policy store, and
/// record-access policy by value to apply the **same** authz the query path
/// enforces when it materializes the artifact later.
struct ResolvedExportAuthz {
    tenant_config: Option<schema_forge_backend::tenant::TenantConfig>,
    policy_store: std::sync::Arc<crate::authz::PolicyStore>,
    record_access_policy:
        Option<std::sync::Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>>,
}

/// The schema's resolved `@export` parameters carried into the async job.
///
/// Groups the three values [`spawn_export_job`] needs from `@export(...)` so the
/// handler passes one config bag rather than a long argument list: the requested
/// `format`, the server-enforced `max_rows` cap, and the `bundle_files` opt-in
/// the ZIP path consults before embedding file-field blobs.
struct ExportJobConfig {
    format: ExportFormat,
    max_rows: u64,
    bundle_files: bool,
}

/// Register an async export job with the [`ExportJobActor`] and return a 202
/// Accepted body carrying the job id, leaving generation + upload to the
/// supervised actor.
///
/// This is reached for non-streamable formats (XLSX, ZIP), an explicit
/// `async: true`, or whenever a caller would otherwise block on a large
/// materialization. A 503 is returned if no storage backend is configured, since
/// the artifact would have nowhere to land. `cfg` carries the schema's resolved
/// `@export` parameters (format, row cap, and the `bundle_files` opt-in the ZIP
/// path consults to decide whether to embed file-field blobs).
async fn spawn_export_job(
    state: &AppState<SchemaForgeConfig>,
    schema_def: &SchemaDefinition,
    cfg: ExportJobConfig,
    body: &ExportRequestBody,
    claims: Option<&Claims>,
    authz: ResolvedExportAuthz,
) -> Result<Response, ForgeError> {
    let ExportJobConfig {
        format,
        max_rows,
        bundle_files,
    } = cfg;
    // The async path generates every declared format (csv, ndjson, xlsx, zip). A
    // format outside that set is refused rather than silently failing the job
    // after accepting it.
    if !async_job_supports(format) {
        return Err(ForgeError::ExportDeferred {
            message: format!(
                "format '{}' is not supported by the async export pipeline; see ADR-0003",
                format.as_str()
            ),
        });
    }

    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");
    let job_actor = state
        .actor::<crate::export_job::ExportJobActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ExportJobActor not registered".into(),
        })?;
    let schema = schema_def.name.as_str();

    // Resolve the storage backend the artifact will land in. Without storage
    // there is no place to write the file, so the async path is unavailable.
    let registry = {
        let (tx, rx) = oneshot::channel();
        forge
            .send(crate::messages::GetStorageRegistry {
                reply: ReplyChannel::new(tx),
            })
            .await;
        ask_forge(rx).await?
    };
    let Some(store) = crate::export_job::pick_export_store(&registry) else {
        return Err(ForgeError::HookUnavailable {
            message: "no storage backend configured for async export artifacts".into(),
        });
    };

    let job_id = crate::export_job::new_job_id();
    let object_key = format!("exports/{}/{}.{}", schema, job_id, format.as_str());

    audit_export(
        state,
        "forge.export.initiated",
        AuditSeverity::Informational,
        serde_json::json!({
            "schema": schema,
            "format": format.as_str(),
            "filter": &body.filter,
            "fields": &body.fields,
            "async": true,
            "job_id": &job_id,
            "user": claims.map(|c| &c.sub),
        }),
    )
    .await;

    let spec = crate::export_job::ExportJobSpec {
        job_id: job_id.clone(),
        schema_name: schema.to_string(),
        schema_def: schema_def.clone(),
        format,
        filter: body.filter.clone(),
        fields: body.fields.clone(),
        max_rows,
        bundle_files,
        claims: claims.cloned(),
        tenant_config: authz.tenant_config,
        policy_store: authz.policy_store,
        record_access_policy: authz.record_access_policy,
        forge: forge.clone(),
        store,
        object_key,
        audit_logger: state.audit_logger().cloned(),
        subject: claims.map(|c| c.sub.clone()),
    };

    job_actor
        .send(crate::export_job::StartExportJob::new(spec))
        .await;

    let response_body = serde_json::json!({
        "job_id": job_id,
        "status": "queued",
        "schema": schema,
        "format": format.as_str(),
    });
    Ok((StatusCode::ACCEPTED, axum::Json(response_body)).into_response())
}

/// `GET /schemas/{schema}/exports/{job_id}` — async export job status.
///
/// Returns the job's current state (`queued` / `running` / `complete` /
/// `failed`) and, once complete, a TTL-bounded presigned GET URL to the
/// generated artifact in object storage. Authorization reuses the distinct
/// `Export{Schema}` Cedar action, so only a principal who could initiate an
/// export can poll its status.
#[instrument(skip_all, fields(schema = %schema, job_id = %job_id))]
pub async fn get_export_job(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, job_id)): Path<(String, String)>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<Response, ForgeError> {
    let schema_name = SchemaName::new(&schema).map_err(|_| ForgeError::InvalidSchemaName {
        name: schema.clone(),
    })?;

    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Resolve schema + enforce the same export gate as the POST path.
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema_def = ask_forge(rx).await?.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;
    if !schema_def.is_exportable() {
        return Err(ForgeError::Forbidden {
            message: format!(
                "schema '{}' is not exportable: no @export annotation",
                schema_name.as_str()
            ),
        });
    }
    let policy_store = crate::routes::entities::fetch_export_policy_store(&state).await?;
    check_export_access(&policy_store, &schema_def, claims.as_ref())?;

    let job_actor = state
        .actor::<crate::export_job::ExportJobActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ExportJobActor not registered".into(),
        })?;
    let (tx, rx) = oneshot::channel();
    job_actor
        .send(crate::export_job::GetExportJob {
            job_id: job_id.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let record = ask_forge(rx).await?;

    let Some(record) = record else {
        return Err(ForgeError::EntityNotFound {
            schema: format!("{}/exports", schema_name.as_str()),
            entity_id: job_id,
        });
    };

    // A job is scoped to the schema in its URL; refuse a job id that belongs to
    // a different schema so one schema's job ids cannot probe another's.
    if record.schema_name != schema_name.as_str() {
        return Err(ForgeError::EntityNotFound {
            schema: format!("{}/exports", schema_name.as_str()),
            entity_id: job_id,
        });
    }

    // Export artifacts are owner-scoped: only the subject that initiated the job
    // may read its status or mint its download URL. Schema-level export access
    // (checked above) is necessary but NOT sufficient — without this gate any
    // caller permitted to export the schema could read another subject's job and
    // its presigned download URL (IDOR / bulk-data exfiltration). Mismatches
    // return EntityNotFound, not Forbidden, so a job id's existence never leaks.
    if !may_read_export_job(
        record.owner_sub.as_deref(),
        claims.as_ref().map(|c| c.sub.as_str()),
    ) {
        return Err(ForgeError::EntityNotFound {
            schema: format!("{}/exports", schema_name.as_str()),
            entity_id: job_id,
        });
    }

    Ok((StatusCode::OK, axum::Json(record.to_status_json().await)).into_response())
}

/// Whether `caller` may read an export job owned by `owner`.
///
/// Export jobs are strictly owner-scoped: the subjects must match exactly. A job
/// created by an authenticated subject is readable only by that same subject; an
/// anonymous job (`owner == None`) is a capability guarded by its unguessable id
/// and is never readable by an authenticated caller who did not create it. Pure
/// and total so the authorization decision is unit-testable in isolation.
fn may_read_export_job(owner: Option<&str>, caller: Option<&str>) -> bool {
    owner == caller
}

/// Resolve `id -> display` maps for the relation columns in `columns`, scoped to
/// the same tenant the caller would see through a list call.
///
/// Returns a `field -> id -> display` map. Only fields that are both
/// `@exportable` (present in `columns`) and relations get resolved; everything
/// else is absent (the serializer then falls back to the raw id).
async fn resolve_export_displays(
    forge: &acton_service::prelude::ActorHandle,
    schema: &SchemaDefinition,
    visible: &[Entity],
    columns: &[(String, Option<schema_forge_core::types::ExportFlatten>)],
    claims: Option<&Claims>,
    tenant_config: &Option<schema_forge_backend::tenant::TenantConfig>,
) -> Result<HashMap<String, HashMap<String, String>>, ForgeError> {
    use std::collections::HashSet;

    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    if visible.is_empty() {
        return Ok(out);
    }

    for (field_name, _) in columns {
        let Some(field) = schema.field(field_name) else {
            continue;
        };
        let FieldType::Relation { target, .. } = &field.field_type else {
            continue;
        };

        // Distinct referenced IDs for this relation column across all rows.
        let mut ids: HashSet<String> = HashSet::new();
        for entity in visible {
            if let Some(value) = entity.field(field_name) {
                collect_ref_ids(value, &mut ids);
            }
        }
        if ids.is_empty() {
            continue;
        }

        // Look up the target schema to find its @display field.
        let (tx, rx) = oneshot::channel();
        forge
            .send(GetSchema {
                name: target.as_str().to_string(),
                reply: ReplyChannel::new(tx),
            })
            .await;
        let Some(target_def) = ask_forge(rx).await? else {
            continue;
        };
        let Some(display_field) = target_def.display_field().map(str::to_string) else {
            continue;
        };

        let id_values: Vec<DynamicValue> = ids.into_iter().map(DynamicValue::Text).collect();
        let mut display_query = Query::new(target_def.id.clone())
            .with_filter(Filter::In {
                path: schema_forge_core::query::FieldPath::single("id"),
                values: id_values,
            })
            .without_total_count();
        display_query.projection = Some(vec!["id".to_string(), display_field.clone()]);
        inject_tenant_scope(&mut display_query, claims, tenant_config);

        let (tx, rx) = oneshot::channel();
        forge
            .send(QueryEntities {
                query: display_query,
                reply: ReplyChannel::new(tx),
            })
            .await;
        let display_result = ask_forge(rx).await?.map_err(ForgeError::from)?;

        let mut id_to_display: HashMap<String, String> = HashMap::new();
        for target_entity in display_result.entities {
            if let Some(value) = target_entity.field(&display_field) {
                id_to_display.insert(target_entity.id.to_string(), display_scalar(value));
            }
        }
        if !id_to_display.is_empty() {
            out.insert(field_name.clone(), id_to_display);
        }
    }

    Ok(out)
}

/// Collect referenced relation IDs from a stored relation value into `out`.
fn collect_ref_ids(value: &DynamicValue, out: &mut std::collections::HashSet<String>) {
    match value {
        DynamicValue::Ref(id) => {
            out.insert(id.to_string());
        }
        DynamicValue::RefArray(ids) => {
            for id in ids {
                out.insert(id.to_string());
            }
        }
        DynamicValue::Text(s) => {
            out.insert(s.clone());
        }
        DynamicValue::Array(items) => {
            for item in items {
                collect_ref_ids(item, out);
            }
        }
        _ => {}
    }
}

/// Stringify a `@display` field value for the relation display map.
fn display_scalar(value: &DynamicValue) -> String {
    match value {
        DynamicValue::Text(s) => s.clone(),
        DynamicValue::Integer(n) => n.to_string(),
        DynamicValue::Float(n) => n.to_string(),
        DynamicValue::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Annotation, ExportFlatten, ExportFormat as EF, FieldAnnotation, FieldDefinition, FieldName,
        FieldType, SchemaId, SchemaName, TextConstraints,
    };
    use std::collections::BTreeMap;

    fn exportable_field(name: &str, flatten: Option<ExportFlatten>) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::Exportable { flatten }],
        )
    }

    fn plain_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    fn export_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Subject").unwrap(),
            vec![
                exportable_field("name", None),
                plain_field("ssn"), // readable but NOT @exportable
                exportable_field("notes", None),
            ],
            vec![Annotation::Export {
                formats: vec![EF::Csv, EF::Ndjson],
                bundle_files: false,
                max_rows: 100,
            }],
        )
        .unwrap()
    }

    fn row(id: &str, fields: &[(&str, &str)]) -> Entity {
        let mut map: BTreeMap<String, DynamicValue> = BTreeMap::new();
        for (k, v) in fields {
            map.insert((*k).to_string(), DynamicValue::Text((*v).to_string()));
        }
        Entity::with_id(
            EntityId::new(id),
            SchemaName::new("Subject").unwrap(),
            map,
        )
    }

    #[test]
    fn streamable_only_csv_and_ndjson() {
        assert!(is_streamable(ExportFormat::Csv));
        assert!(is_streamable(ExportFormat::Ndjson));
        assert!(!is_streamable(ExportFormat::Xlsx));
        assert!(!is_streamable(ExportFormat::Zip));
    }

    #[test]
    fn async_job_supports_all_declared_formats() {
        // CSV/NDJSON also stream; XLSX and ZIP are async-only (buffered). All four
        // declared formats are materializable on the async-job path.
        assert!(async_job_supports(ExportFormat::Csv));
        assert!(async_job_supports(ExportFormat::Ndjson));
        assert!(async_job_supports(ExportFormat::Xlsx));
        assert!(async_job_supports(ExportFormat::Zip));
    }

    /// Read the first worksheet of an in-memory `.xlsx` back into a grid of
    /// stringified cells so a test can assert the workbook is well-formed without
    /// touching the filesystem.
    fn read_xlsx_grid(bytes: &[u8]) -> Vec<Vec<String>> {
        use calamine::{Data, Reader, Xlsx};
        use std::io::Cursor;

        let mut workbook: Xlsx<_> =
            calamine::open_workbook_from_rs(Cursor::new(bytes.to_vec())).expect("valid xlsx");
        let sheet_name = workbook.sheet_names()[0].clone();
        let range = workbook.worksheet_range(&sheet_name).expect("first sheet");
        range
            .rows()
            .map(|r| {
                r.iter()
                    .map(|c| match c {
                        Data::Empty => String::new(),
                        Data::String(s) => s.clone(),
                        Data::Float(f) => f.to_string(),
                        Data::Int(i) => i.to_string(),
                        Data::Bool(b) => b.to_string(),
                        Data::DateTime(d) => d.to_string(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn xlsx_header_and_rows_use_exportable_columns_only() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let rows = vec![
            row("a", &[("name", "Ada"), ("ssn", "111-22-3333"), ("notes", "vip")]),
            row("b", &[("name", "Bob"), ("ssn", "999-88-7777"), ("notes", "x,y")]),
        ];
        let bytes = entities_to_xlsx(&rows, &cols, &HashMap::new()).unwrap();

        // A valid xlsx is a ZIP archive: it starts with the PK signature.
        assert_eq!(&bytes[..2], b"PK", "not a zip/xlsx container");

        let grid = read_xlsx_grid(&bytes);
        assert_eq!(grid[0], vec!["name", "notes"]);
        assert_eq!(grid[1], vec!["Ada", "vip"]);
        // The comma inside a cell rides as-is in a real spreadsheet cell.
        assert_eq!(grid[2], vec!["Bob", "x,y"]);
        // SSN is readable-but-not-@exportable: it must never appear.
        let flat = grid.concat().join("|");
        assert!(!flat.contains("111-22-3333"), "leaked secret: {flat}");
        assert!(!flat.contains("999-88-7777"), "leaked secret: {flat}");
    }

    #[test]
    fn xlsx_stripped_field_renders_empty_cell() {
        // A row missing `notes` (stripped for read access or simply absent)
        // keeps the column header and renders an empty cell — never another
        // row's value.
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let rows = vec![row("a", &[("name", "Ada")])];
        let bytes = entities_to_xlsx(&rows, &cols, &HashMap::new()).unwrap();
        let grid = read_xlsx_grid(&bytes);
        assert_eq!(grid[0], vec!["name", "notes"]);
        // calamine trims trailing empty cells, so the data row is just "Ada".
        assert_eq!(grid[1].first().map(String::as_str), Some("Ada"));
        assert!(grid[1].get(1).map(String::as_str).unwrap_or("").is_empty());
    }

    #[test]
    fn xlsx_empty_entities_yields_header_only() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let bytes = entities_to_xlsx(&[], &cols, &HashMap::new()).unwrap();
        let grid = read_xlsx_grid(&bytes);
        assert_eq!(grid.len(), 1);
        assert_eq!(grid[0], vec!["name", "notes"]);
    }

    #[test]
    fn xlsx_numbers_and_booleans_are_native_typed() {
        // Build a schema with a numeric + bool exportable column to confirm the
        // cells round-trip as native (non-string) spreadsheet values.
        use schema_forge_core::types::IntegerConstraints;
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Metric").unwrap(),
            vec![
                FieldDefinition::with_annotations(
                    FieldName::new("count").unwrap(),
                    FieldType::Integer(IntegerConstraints::unconstrained()),
                    vec![],
                    vec![FieldAnnotation::Exportable { flatten: None }],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("active").unwrap(),
                    FieldType::Boolean,
                    vec![],
                    vec![FieldAnnotation::Exportable { flatten: None }],
                ),
            ],
            vec![Annotation::Export {
                formats: vec![EF::Xlsx],
                bundle_files: false,
                max_rows: 100,
            }],
        )
        .unwrap();
        let cols = resolve_export_columns(&schema, None);

        let mut map: BTreeMap<String, DynamicValue> = BTreeMap::new();
        map.insert("count".into(), DynamicValue::Integer(42));
        map.insert("active".into(), DynamicValue::Boolean(true));
        let entity = Entity::with_id(EntityId::new("m1"), SchemaName::new("Metric").unwrap(), map);

        let bytes = entities_to_xlsx(&[entity], &cols, &HashMap::new()).unwrap();

        use calamine::{Data, Reader, Xlsx};
        use std::io::Cursor;
        let mut wb: Xlsx<_> =
            calamine::open_workbook_from_rs(Cursor::new(bytes)).expect("valid xlsx");
        let sheet = wb.sheet_names()[0].clone();
        let range = wb.worksheet_range(&sheet).unwrap();
        // Row 1 (after header): count is a native number, active a native bool.
        assert_eq!(range.get_value((1, 0)), Some(&Data::Float(42.0)));
        assert_eq!(range.get_value((1, 1)), Some(&Data::Bool(true)));
    }

    #[test]
    fn columns_are_only_exportable_fields() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
        // `ssn` is readable but not @exportable, so it must NOT appear.
        assert_eq!(names, vec!["name", "notes"]);
    }

    #[test]
    fn requested_fields_narrow_but_never_widen() {
        let schema = export_schema();
        // Caller asks for `ssn` (not exportable) and `name` (exportable).
        let req = vec!["ssn".to_string(), "name".to_string()];
        let cols = resolve_export_columns(&schema, Some(&req));
        let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
        // Only the exportable intersection survives: `ssn` is dropped.
        assert_eq!(names, vec!["name"]);
    }

    #[test]
    fn requested_unknown_field_is_ignored() {
        let schema = export_schema();
        let req = vec!["does_not_exist".to_string()];
        let cols = resolve_export_columns(&schema, Some(&req));
        assert!(cols.is_empty());
    }

    #[test]
    fn csv_header_and_rows_use_exportable_columns_only() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let rows = vec![
            row("a", &[("name", "Ada"), ("ssn", "111-22-3333"), ("notes", "vip")]),
            row("b", &[("name", "Bob"), ("ssn", "999-88-7777"), ("notes", "x,y")]),
        ];
        let csv = entities_to_csv(&rows, &cols, &HashMap::new()).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "name,notes");
        assert_eq!(lines.next().unwrap(), "Ada,vip");
        // The comma inside the cell must be quoted by the csv writer.
        assert_eq!(lines.next().unwrap(), "Bob,\"x,y\"");
        // SSN never appears anywhere in the file.
        assert!(!csv.contains("111-22-3333"));
        assert!(!csv.contains("999-88-7777"));
    }

    #[test]
    fn csv_stripped_field_renders_empty_cell() {
        // Simulate a row where field-access stripped `notes`: it is simply
        // absent from the entity. The column stays in the header; the cell is
        // empty — fail-closed, never a leak from another row.
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let rows = vec![row("a", &[("name", "Ada")])]; // no `notes`
        let csv = entities_to_csv(&rows, &cols, &HashMap::new()).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "name,notes");
        assert_eq!(lines.next().unwrap(), "Ada,");
    }

    #[test]
    fn ndjson_emits_id_and_exportable_columns_only() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let rows = vec![row(
            "a",
            &[("name", "Ada"), ("ssn", "111-22-3333"), ("notes", "vip")],
        )];
        let ndjson = entities_to_ndjson(&rows, &cols, &HashMap::new());
        let line = ndjson.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["name"], serde_json::json!("Ada"));
        assert_eq!(v["notes"], serde_json::json!("vip"));
        assert!(v.get("id").is_some());
        // SSN is not @exportable, so it must be absent.
        assert!(v.get("ssn").is_none());
    }

    #[test]
    fn ndjson_absent_field_is_omitted_not_null() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let rows = vec![row("a", &[("name", "Ada")])]; // no `notes`
        let ndjson = entities_to_ndjson(&rows, &cols, &HashMap::new());
        let v: serde_json::Value = serde_json::from_str(ndjson.lines().next().unwrap()).unwrap();
        assert_eq!(v["name"], serde_json::json!("Ada"));
        assert!(v.get("notes").is_none());
    }

    #[test]
    fn csv_empty_entities_yields_header_only() {
        let schema = export_schema();
        let cols = resolve_export_columns(&schema, None);
        let csv = entities_to_csv(&[], &cols, &HashMap::new()).unwrap();
        assert_eq!(csv.lines().count(), 1);
        assert_eq!(csv.lines().next().unwrap(), "name,notes");
    }

    #[test]
    fn export_job_owner_may_read_own() {
        assert!(may_read_export_job(Some("alice"), Some("alice")));
    }

    #[test]
    fn export_job_other_subject_denied() {
        // The IDOR: a different authenticated subject must not read alice's job,
        // even though they too can export the schema.
        assert!(!may_read_export_job(Some("alice"), Some("bob")));
    }

    #[test]
    fn export_job_anonymous_caller_denied_owned_job() {
        assert!(!may_read_export_job(Some("alice"), None));
    }

    #[test]
    fn export_job_authenticated_caller_denied_anonymous_job() {
        assert!(!may_read_export_job(None, Some("bob")));
    }

    #[test]
    fn export_job_anonymous_capability_matches() {
        // An anonymous job is a capability guarded solely by its unguessable id.
        assert!(may_read_export_job(None, None));
    }
}
