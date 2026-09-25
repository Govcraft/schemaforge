//! Integration tests for the synchronous streamable export endpoint
//! (`POST /schemas/{schema}/entities/export`, item 5 of the export epic).
//!
//! In-process SurrealDB (`mem://`), no external services. Exercises the
//! fail-closed gates and the AU-2 audit-shaped flow end to end: a schema without
//! `@export` refuses; a non-`@exportable` field never appears in the file; the
//! read-vs-export Cedar split denies a read-only role; the row cap defers to the
//! async path; and CSV/NDJSON bodies stream inline with the right content type.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use acton_service::config::Config;
use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_acton::messages::{InitForge, ReplyChannel};
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::DynForgeBackend;
use schema_forge_acton::DynSchemaBackend;
use schema_forge_acton::ForgeActor;
use schema_forge_core::types::{
    Annotation, ExportFormat, FieldAnnotation, FieldDefinition, FieldName, FieldType,
    SchemaDefinition, SchemaId, SchemaName, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use tokio::sync::oneshot;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn make_claims(roles: &[&str]) -> Claims {
    Claims {
        sub: "user:test-user".to_string(),
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

async fn build_state_with_config(
    backend: Arc<dyn DynForgeBackend>,
    registry: HashMap<String, SchemaDefinition>,
    custom: SchemaForgeConfig,
) -> AppState<SchemaForgeConfig> {
    use acton_service::service_builder::ServiceBuilder;

    let config = Config::<SchemaForgeConfig> {
        custom,
        ..Default::default()
    };
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .with_actor::<schema_forge_acton::HookDispatchActor>()
        .with_actor::<schema_forge_acton::ExportJobActor>()
        .with_actor::<schema_forge_acton::ExportRateLimiter>()
        .build();

    let forge_handle = service
        .state()
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    let (tx, rx) = oneshot::channel();
    forge_handle
        .send(InitForge {
            registry,
            backend,
            tenant_config: None,
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: schema_forge_acton::storage::StorageRegistry::default(),
            policy_store: None,
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("InitForge timeout")
        .expect("InitForge channel dropped");

    service.state().clone()
}

fn app_with_claims(state: AppState<SchemaForgeConfig>, claims: Claims) -> Router {
    forge_routes()
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let claims = claims.clone();
                async move {
                    req.extensions_mut().insert(claims);
                    next.run(req).await
                }
            },
        ))
        .with_state(state)
}

/// Send a JSON request and return `(status, parsed-json-or-null)`.
async fn json_request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let (status, _ctype, bytes) = raw_request(app, method, path, body).await;
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

/// Send a request and return `(status, content-type, raw body bytes)`.
async fn raw_request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, String, Vec<u8>) {
    let body = match body {
        Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
        None => Body::empty(),
    };
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let ctype = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, ctype, bytes.to_vec())
}

fn exportable_field(name: &str) -> FieldDefinition {
    FieldDefinition::with_annotations(
        FieldName::new(name).unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
        vec![],
        vec![FieldAnnotation::Exportable { flatten: None }],
    )
}

fn plain_field(name: &str) -> FieldDefinition {
    FieldDefinition::new(
        FieldName::new(name).unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    )
}

/// A `Subject` schema: `name` + `notes` are `@exportable`, `secret` is readable
/// but not exportable. `read` is granted to the `analyst` role; export has no
/// permit so the read-only role is denied export.
fn export_schema(max_rows: u64) -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Subject").unwrap(),
        vec![
            exportable_field("name"),
            exportable_field("notes"),
            plain_field("secret"),
        ],
        vec![
            Annotation::Access {
                read: vec!["analyst".to_string()],
                write: vec!["analyst".to_string()],
                delete: vec![],
                cross_tenant_read: vec![],
            },
            Annotation::Export {
                formats: vec![ExportFormat::Csv, ExportFormat::Ndjson],
                bundle_files: false,
                max_rows,
            },
        ],
    )
    .unwrap()
}

/// A schema with NO `@export` annotation: export must refuse.
fn non_export_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Locked").unwrap(),
        vec![exportable_field("name")],
        vec![Annotation::Access {
            read: vec!["analyst".to_string()],
            write: vec!["analyst".to_string()],
            delete: vec![],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap()
}

async fn provision(
    backend: &Arc<SurrealBackend>,
    schema: &SchemaDefinition,
) {
    let plan = schema_forge_core::migration::DiffEngine::create_new(schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("migration");
    backend
        .store_schema_metadata(schema)
        .await
        .expect("metadata");
}

/// Build an app pre-seeded with the given schema and seed rows (created as
/// platform_admin), then return an app bound to `caller_roles`.
async fn seeded_app(
    schema: SchemaDefinition,
    rows: &[serde_json::Value],
    caller_roles: &[&str],
) -> Router {
    seeded_app_with_config(schema, rows, caller_roles, SchemaForgeConfig::default()).await
}

/// Like [`seeded_app`] but with operator-tuned `[schema_forge.export]` settings,
/// so a test can exercise the server-wide row ceiling and the rate limit.
async fn seeded_app_with_config(
    schema: SchemaDefinition,
    rows: &[serde_json::Value],
    caller_roles: &[&str],
    custom: SchemaForgeConfig,
) -> Router {
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .expect("mem surreal"),
    );
    provision(&backend, &schema).await;

    let mut registry = HashMap::new();
    registry.insert(schema.name.as_str().to_string(), schema.clone());
    let state = build_state_with_config(backend, registry, custom).await;

    // Seed rows as platform_admin (bypasses access checks).
    let admin = app_with_claims(state.clone(), make_claims(&["platform_admin"]));
    let path = format!("/schemas/{}/entities", schema.name.as_str());
    for row in rows {
        let (status, json) =
            json_request(&admin, Method::POST, &path, Some(row.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "seed failed: {json}");
    }

    app_with_claims(state, make_claims(caller_roles))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn csv_export_streams_only_exportable_columns() {
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "vip", "secret": "SSN-111" } }),
        serde_json::json!({ "fields": { "name": "Bob", "notes": "x,y", "secret": "SSN-222" } }),
    ];
    let app = seeded_app(export_schema(100), &rows, &["platform_admin"]).await;

    let (status, ctype, body) = raw_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {}", String::from_utf8_lossy(&body));
    assert!(ctype.starts_with("text/csv"), "content-type: {ctype}");
    let text = String::from_utf8(body).unwrap();
    let header = text.lines().next().unwrap();
    // Only @exportable columns, in declaration order; `secret` excluded.
    assert_eq!(header, "name,notes");
    // The non-exportable `secret` value must never appear in the file.
    assert!(!text.contains("SSN-111"), "leaked secret: {text}");
    assert!(!text.contains("SSN-222"), "leaked secret: {text}");
    // The comma inside a cell is quoted.
    assert!(text.contains("\"x,y\""), "csv quoting wrong: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ndjson_export_streams_id_and_exportable_columns() {
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "vip", "secret": "SSN-111" } }),
    ];
    let app = seeded_app(export_schema(100), &rows, &["platform_admin"]).await;

    let (status, ctype, body) = raw_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "ndjson" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {}", String::from_utf8_lossy(&body));
    assert_eq!(ctype, "application/x-ndjson", "content-type: {ctype}");
    let text = String::from_utf8(body).unwrap();
    let line = text.lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["name"], serde_json::json!("Ada"));
    assert_eq!(v["notes"], serde_json::json!("vip"));
    assert!(v.get("id").is_some());
    assert!(v.get("secret").is_none(), "leaked secret in ndjson: {line}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_requested_fields_narrow_to_intersection() {
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "vip", "secret": "SSN-111" } }),
    ];
    let app = seeded_app(export_schema(100), &rows, &["platform_admin"]).await;

    // Caller asks for `secret` (not exportable) and `name` (exportable).
    let (status, _ct, body) = raw_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv", "fields": ["secret", "name"] })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).unwrap();
    // Only the exportable intersection: `name`. `secret` is dropped.
    assert_eq!(text.lines().next().unwrap(), "name");
    assert!(!text.contains("SSN-111"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_refused_when_schema_has_no_export_annotation() {
    let rows = [serde_json::json!({ "fields": { "name": "Ada" } })];
    let app = seeded_app(non_export_schema(), &rows, &["platform_admin"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Locked/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    // Fail-closed: even a platform_admin cannot export a schema with no @export.
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_role_is_denied_export() {
    // The `analyst` role is granted read+write by @access, but export has its
    // own Cedar action with no permit, so export-many is denied (ADR-0003).
    let rows = [serde_json::json!({ "fields": { "name": "Ada", "notes": "n" } })];
    let app = seeded_app(export_schema(100), &rows, &["analyst"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_cap_export_is_deferred_with_413() {
    // Cap of 1; seed 2 rows so the result strictly exceeds the cap.
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "a" } }),
        serde_json::json!({ "fields": { "name": "Bob", "notes": "b" } }),
    ];
    let app = seeded_app(export_schema(1), &rows, &["platform_admin"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body: {json}");
    assert_eq!(json["error"], "export_too_large");
    assert_eq!(json["max_rows"], serde_json::json!(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xlsx_format_takes_async_path() {
    // Item 7 added the XLSX serializer on the async path: xlsx is always
    // async-only (rust_xlsxwriter buffers the whole workbook), so a POST for it
    // takes the supervised async-job path rather than being flatly deferred. With
    // no storage backend configured in this harness the artifact has nowhere to
    // land, so the request is refused with 503 — NOT a 422 `export_deferred`. The
    // full accept + generated-workbook lifecycle is covered with a mock store in
    // `export_async_integration.rs`.
    let rows = [serde_json::json!({ "fields": { "name": "Ada", "notes": "n" } })];
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Subject").unwrap(),
        vec![exportable_field("name"), exportable_field("notes")],
        vec![
            Annotation::Access {
                read: vec!["analyst".to_string()],
                write: vec!["analyst".to_string()],
                delete: vec![],
                cross_tenant_read: vec![],
            },
            Annotation::Export {
                formats: vec![ExportFormat::Csv, ExportFormat::Xlsx],
                bundle_files: false,
                max_rows: 100,
            },
        ],
    )
    .unwrap();
    let app = seeded_app(schema, &rows, &["platform_admin"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "xlsx" })),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {json}");
    assert_ne!(json["error"], "export_deferred");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_async_true_without_storage_is_unavailable() {
    // Item 6 added the async-job path: an explicit `async: true` now takes that
    // path rather than being flatly deferred. With no storage backend configured
    // in this harness the artifact has nowhere to land, so the request is refused
    // with 503 (fail-closed) instead of accepted. The full accept + lifecycle is
    // covered by `export_async_integration.rs` with a mock store.
    let rows = [serde_json::json!({ "fields": { "name": "Ada", "notes": "n" } })];
    let app = seeded_app(export_schema(100), &rows, &["platform_admin"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv", "async": true })),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {json}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn format_not_in_vocabulary_is_forbidden() {
    // Schema only declares csv; a request for ndjson must be refused.
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Subject").unwrap(),
        vec![exportable_field("name")],
        vec![
            Annotation::Access {
                read: vec!["analyst".to_string()],
                write: vec!["analyst".to_string()],
                delete: vec![],
                cross_tenant_read: vec![],
            },
            Annotation::Export {
                formats: vec![ExportFormat::Csv],
                bundle_files: false,
                max_rows: 100,
            },
        ],
    )
    .unwrap();
    let rows = [serde_json::json!({ "fields": { "name": "Ada" } })];
    let app = seeded_app(schema, &rows, &["platform_admin"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "ndjson" })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

/// Build a `SchemaForgeConfig` with an export row ceiling and rate limit set.
fn export_config(default_max_rows: u64, max_requests: u32, window_secs: u64) -> SchemaForgeConfig {
    use schema_forge_acton::export_config::{ExportRateLimitSettings, ExportSettings};
    let mut custom = SchemaForgeConfig::default();
    custom.schema_forge.export = ExportSettings {
        default_max_rows,
        rate_limit: ExportRateLimitSettings {
            max_requests,
            window_secs,
        },
    };
    custom
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_ceiling_clamps_below_generous_schema_cap() {
    // The schema declares a generous 1000-row cap, but the operator pins a
    // server-wide ceiling of 1. Two rows then exceed the *resolved* cap and the
    // export is refused 413 — the schema cannot widen the operator's bound.
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "a" } }),
        serde_json::json!({ "fields": { "name": "Bob", "notes": "b" } }),
    ];
    let app = seeded_app_with_config(
        export_schema(1000),
        &rows,
        &["platform_admin"],
        export_config(1, 100, 60),
    )
    .await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body: {json}");
    assert_eq!(json["error"], "export_too_large");
    // The resolved cap reported is the server ceiling, not the schema's 1000.
    assert_eq!(json["max_rows"], serde_json::json!(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tighter_schema_cap_wins_under_generous_server_ceiling() {
    // The inverse: schema caps at 1, server ceiling is generous (1000). Two rows
    // still exceed the schema's tighter cap — the entity can override downward.
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "a" } }),
        serde_json::json!({ "fields": { "name": "Bob", "notes": "b" } }),
    ];
    let app = seeded_app_with_config(
        export_schema(1),
        &rows,
        &["platform_admin"],
        export_config(1000, 100, 60),
    )
    .await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body: {json}");
    assert_eq!(json["max_rows"], serde_json::json!(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_rejects_after_allowance_exhausted() {
    // Allow exactly 2 exports per (long) window. The 3rd is rejected 429.
    let rows = [serde_json::json!({ "fields": { "name": "Ada", "notes": "n" } })];
    let app = seeded_app_with_config(
        export_schema(100),
        &rows,
        &["platform_admin"],
        export_config(100, 2, 3600),
    )
    .await;

    for i in 1..=2 {
        let (status, _json) = json_request(
            &app,
            Method::POST,
            "/schemas/Subject/entities/export",
            Some(serde_json::json!({ "format": "csv" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "export {i} should be admitted");
    }

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {json}");
    assert_eq!(json["error"], "rate_limited");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_zero_requests_is_a_kill_switch() {
    // A rate limit of 0 admits nobody: every export is refused 429 (fail-closed).
    let rows = [serde_json::json!({ "fields": { "name": "Ada", "notes": "n" } })];
    let app = seeded_app_with_config(
        export_schema(100),
        &rows,
        &["platform_admin"],
        export_config(100, 0, 60),
    )
    .await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {json}");
    assert_eq!(json["error"], "rate_limited");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requesting_only_non_exportable_fields_is_denied() {
    // A caller asking exclusively for a readable-but-not-@exportable field is a
    // denied path (audited as `no_exportable_fields_requested`), not a silent
    // id-only stream. `secret` is readable but never exportable.
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "n", "secret": "SSN-111" } }),
    ];
    let app = seeded_app(export_schema(100), &rows, &["platform_admin"]).await;

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv", "fields": ["secret"] })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {json}");
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_non_exportable_request_still_narrows_and_succeeds() {
    // Asking for one exportable (`name`) and one non-exportable (`secret`) field
    // is NOT a denial — it narrows to the exportable intersection and streams.
    let rows = [
        serde_json::json!({ "fields": { "name": "Ada", "notes": "n", "secret": "SSN-111" } }),
    ];
    let app = seeded_app(export_schema(100), &rows, &["platform_admin"]).await;

    let (status, _ct, body) = raw_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv", "fields": ["secret", "name"] })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).unwrap();
    assert_eq!(text.lines().next().unwrap(), "name");
    assert!(!text.contains("SSN-111"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_result_streams_header_only_csv() {
    let app = seeded_app(export_schema(100), &[], &["platform_admin"]).await;

    let (status, ctype, body) = raw_request(
        &app,
        Method::POST,
        "/schemas/Subject/entities/export",
        Some(serde_json::json!({ "format": "csv" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(ctype.starts_with("text/csv"));
    let text = String::from_utf8(body).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert_eq!(text.lines().next().unwrap(), "name,notes");
}

struct ExportTargetPolicy;

impl schema_forge_backend::auth::RecordAccessPolicy for ExportTargetPolicy {
    fn filter_visible<'a>(
        &'a self,
        _schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<schema_forge_backend::entity::Entity>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Vec<schema_forge_backend::entity::Entity>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            assert_eq!(claims.sub, "user:test-user");
            entities
                .into_iter()
                .filter(|entity| {
                    entity.field("label")
                        != Some(&schema_forge_core::types::DynamicValue::Text(
                            "operator-secret".into(),
                        ))
                })
                .collect()
        })
    }

    fn can_modify<'a>(
        &'a self,
        _schema: &'a SchemaDefinition,
        _claims: &'a Claims,
        _entity: &'a schema_forge_backend::entity::Entity,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }

    fn can_delete<'a>(
        &'a self,
        _schema: &'a SchemaDefinition,
        _claims: &'a Claims,
        _entity: &'a schema_forge_backend::entity::Entity,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}

#[tokio::test]
async fn export_relation_labels_respect_target_schema_row_field_and_operator_access() {
    use schema_forge_acton::authz::{
        PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks,
    };
    use schema_forge_acton::routes::export::{materialize_export, prepare_export, ExportContext};

    let schemas = schema_forge_dsl::parse(
        r#"
        @access(read: ["manager"])
        @display("label")
        schema RestrictedTarget { label: text }
        @access(read: ["viewer"])
        @display("label")
        schema FieldTarget { label: text @field_access(read: ["manager"]) }
        @access(read: ["viewer"])
        @display("label")
        schema HiddenTarget { label: text @hidden }
        @access(read: ["viewer"])
        @display("label")
        schema RowTarget { label: text blocked: boolean }
        @access(read: ["viewer"])
        @display("label")
        schema VisibleTarget { label: text }
        @access(read: ["viewer"])
        schema ExportLinks {
            denied: -> RestrictedTarget @exportable
            field_denied: -> FieldTarget @exportable
            hidden: -> HiddenTarget @exportable
            row_denied: -> RowTarget @exportable
            allowed: -> RowTarget @exportable
            many: -> RowTarget[] @exportable
            operator_denied: -> VisibleTarget @exportable
        }
    "#,
    )
    .unwrap();
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "export-targets")
            .await
            .unwrap(),
    );
    for schema in &schemas {
        provision(&backend, schema).await;
    }
    let registry = schemas
        .iter()
        .map(|schema| (schema.name.to_string(), schema.clone()))
        .collect();
    let state = build_state_with_config(backend, registry, SchemaForgeConfig::default()).await;
    let admin = app_with_claims(state.clone(), make_claims(&["platform_admin"]));
    let mut ids = HashMap::new();
    for (key, schema, fields) in [
        (
            "denied",
            "RestrictedTarget",
            serde_json::json!({"label": "schema-secret"}),
        ),
        (
            "field_denied",
            "FieldTarget",
            serde_json::json!({"label": "field-secret"}),
        ),
        (
            "hidden",
            "HiddenTarget",
            serde_json::json!({"label": "hidden-secret"}),
        ),
        (
            "row_denied",
            "RowTarget",
            serde_json::json!({"label": "row-secret", "blocked": true}),
        ),
        (
            "allowed",
            "RowTarget",
            serde_json::json!({"label": "readable-label", "blocked": false}),
        ),
        (
            "operator_denied",
            "VisibleTarget",
            serde_json::json!({"label": "operator-secret"}),
        ),
    ] {
        let (status, result) = json_request(
            &admin,
            Method::POST,
            &format!("/schemas/{schema}/entities"),
            Some(serde_json::json!({"fields": fields})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{result}");
        ids.insert(key, result["id"].as_str().unwrap().to_string());
    }
    let mut fields = serde_json::Map::new();
    for (key, id) in &ids {
        fields.insert((*key).into(), serde_json::json!(id));
    }
    fields.insert(
        "many".into(),
        serde_json::json!([ids["row_denied"], ids["allowed"]]),
    );
    let (status, result) = json_request(
        &admin,
        Method::POST,
        "/schemas/ExportLinks/entities",
        Some(serde_json::json!({"fields": fields})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{result}");

    let policies = tempfile::tempdir().unwrap();
    std::fs::write(
        policies.path().join("rows.cedar"),
        r#"
        forbid(principal, action == Action::"ReadRowTarget", resource is RowTarget)
        when { !context.resource_is_placeholder && resource has blocked && resource.blocked };
    "#,
    )
    .unwrap();
    let policy_store = Arc::new(PolicyStore::new(
        PolicyStoreSnapshot::from_schemas(
            &schemas,
            Some(policies.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap(),
    ));
    let record_policy: Option<Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>> =
        Some(Arc::new(ExportTargetPolicy));
    let forge = state.actor::<ForgeActor>().unwrap();
    let claims = make_claims(&["viewer"]);
    let tenant_config = None;
    let context = ExportContext {
        forge: &forge,
        claims: Some(&claims),
        tenant_config: &tenant_config,
        policy_store: &policy_store,
        record_access_policy: &record_policy,
    };
    let schema = schemas
        .iter()
        .find(|schema| schema.name.as_str() == "ExportLinks")
        .unwrap();
    let prepared = prepare_export(&context, schema, None, None, 100)
        .await
        .unwrap();
    assert_eq!(prepared.entities.len(), 1);
    for field in [
        "denied",
        "field_denied",
        "hidden",
        "row_denied",
        "operator_denied",
    ] {
        assert!(
            !prepared.display_map.contains_key(field),
            "unauthorized label for {field}"
        );
    }
    assert_eq!(
        prepared.display_map["allowed"][&ids["allowed"]],
        "readable-label"
    );
    assert_eq!(prepared.display_map["many"].len(), 1);
    assert_eq!(
        prepared.display_map["many"][&ids["allowed"]],
        "readable-label"
    );

    for format in [ExportFormat::Csv, ExportFormat::Ndjson, ExportFormat::Xlsx] {
        let artifact = materialize_export(&context, schema, format, None, None, 100)
            .await
            .unwrap();
        let text = if format == ExportFormat::Xlsx {
            use std::io::Read;
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(artifact.bytes)).unwrap();
            let mut text = String::new();
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).unwrap();
                if entry.name().ends_with(".xml") {
                    entry.read_to_string(&mut text).unwrap();
                }
            }
            text
        } else {
            String::from_utf8(artifact.bytes).unwrap()
        };
        assert!(text.contains("readable-label"));
        for secret in [
            "schema-secret",
            "field-secret",
            "hidden-secret",
            "row-secret",
            "operator-secret",
        ] {
            assert!(
                !text.contains(secret),
                "unauthorized label in {format:?}: {secret}"
            );
        }
    }
}
