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

async fn build_state(
    backend: Arc<dyn DynForgeBackend>,
    registry: HashMap<String, SchemaDefinition>,
) -> AppState<SchemaForgeConfig> {
    use acton_service::service_builder::ServiceBuilder;

    let config = Config::<SchemaForgeConfig>::default();
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .with_actor::<schema_forge_acton::HookDispatchActor>()
        .with_actor::<schema_forge_acton::ExportJobActor>()
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
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .expect("mem surreal"),
    );
    provision(&backend, &schema).await;

    let mut registry = HashMap::new();
    registry.insert(schema.name.as_str().to_string(), schema.clone());
    let state = build_state(backend, registry).await;

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
async fn xlsx_format_is_deferred_to_async_path() {
    let rows = [serde_json::json!({ "fields": { "name": "Ada", "notes": "n" } })];
    // Declare xlsx in the formats vocabulary so the format gate passes and the
    // streamability gate is what defers it.
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

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {json}");
    assert_eq!(json["error"], "export_deferred");
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
