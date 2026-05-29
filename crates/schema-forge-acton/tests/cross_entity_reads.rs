//! Integration tests for constrained single-hop cross-entity reads in
//! `@require` (#95).
//!
//! These exercise the full HTTP → route → prefetch-and-bind → pure-engine path
//! against an in-memory SurrealDB backend. They assert:
//! - a `related.<ref>.<col>` `@require` PASSES when the related row satisfies it
//!   and is REJECTED (422) when it does not;
//! - fail-closed when the FK is null / the related row is missing;
//! - tenant isolation (a related row in another tenant is not readable);
//! - multi-hop and to-many give clear errors (enforced by the runtime resolver
//!   here, since these schemas are built programmatically and bypass the DSL
//!   apply-time guard).

use std::collections::HashMap;
use std::sync::Arc;

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
use schema_forge_acton::ForgeActor;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_backend::SchemaBackend;
use schema_forge_core::types::{
    Annotation, Cardinality, EnumVariants, FieldAnnotation, FieldDefinition, FieldName, FieldType,
    SchemaDefinition, SchemaId, SchemaName, TenantKind, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use tokio::sync::oneshot;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn claims(roles: &[&str]) -> Claims {
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

fn claims_in_tenant(roles: &[&str], tenant_entity_id: &str) -> Claims {
    let mut c = claims(roles);
    c.custom.insert(
        "tenant_chain".to_string(),
        serde_json::json!([{"schema": "Organization", "entity_id": tenant_entity_id}]),
    );
    c
}

async fn build_state(
    backend: Arc<dyn DynForgeBackend>,
    registry: HashMap<String, SchemaDefinition>,
    tenant_config: Option<TenantConfig>,
) -> AppState<SchemaForgeConfig> {
    use acton_service::service_builder::ServiceBuilder;

    let config = Config::<SchemaForgeConfig>::default();
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .with_actor::<schema_forge_acton::HookDispatchActor>()
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
            tenant_config,
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: schema_forge_acton::storage::StorageRegistry::default(),
            policy_store: None,
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;

    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
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

async fn json_request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
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
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn require(expr: &str, message: &str) -> FieldAnnotation {
    FieldAnnotation::Require {
        expr: expr.to_string(),
        message: message.to_string(),
    }
}

fn text_field(name: &str) -> FieldDefinition {
    FieldDefinition::new(
        FieldName::new(name).unwrap(),
        FieldType::Text(TextConstraints::unconstrained()),
    )
}

fn relation_field(name: &str, target: &str, cardinality: Cardinality) -> FieldDefinition {
    FieldDefinition::new(
        FieldName::new(name).unwrap(),
        FieldType::Relation {
            target: SchemaName::new(target).unwrap(),
            cardinality,
        },
    )
}

async fn apply_and_register(
    backend: &Arc<SurrealBackend>,
    registry: &mut HashMap<String, SchemaDefinition>,
    schema: SchemaDefinition,
) {
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("apply migration");
    backend
        .store_schema_metadata(&schema)
        .await
        .expect("store metadata");
    registry.insert(schema.name.as_str().to_string(), schema);
}

/// An `Approval` schema (with a `state` enum) and a `Document` schema whose
/// `@require` reads `related.approval.state`. `doc_require` is the rule body and
/// `approval_extra` lets a test add a second relation on Approval (for the
/// multi-hop case).
fn approval_schema(extra_fields: Vec<FieldDefinition>) -> SchemaDefinition {
    let mut fields = vec![
        text_field("name"),
        FieldDefinition::new(
            FieldName::new("state").unwrap(),
            FieldType::Enum(EnumVariants::new(vec!["pending".into(), "granted".into()]).unwrap()),
        ),
    ];
    fields.extend(extra_fields);
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Approval").unwrap(),
        fields,
        no_access_annotations(),
    )
    .unwrap()
}

fn document_schema(require_expr: &str, approval_cardinality: Cardinality) -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Document").unwrap(),
        vec![
            text_field("title"),
            relation_field("approval", "Approval", approval_cardinality),
            FieldDefinition::with_annotations(
                FieldName::new("status").unwrap(),
                FieldType::Enum(EnumVariants::new(vec!["draft".into(), "closed".into()]).unwrap()),
                vec![],
                vec![require(
                    require_expr,
                    "closed documents need a granted approval",
                )],
            ),
        ],
        no_access_annotations(),
    )
    .unwrap()
}

fn no_access_annotations() -> Vec<Annotation> {
    vec![Annotation::Access {
        read: vec![],
        write: vec![],
        delete: vec![],
        cross_tenant_read: vec![],
    }]
}

const REQUIRE_GRANTED: &str = "status != 'closed' || related.approval.state == 'granted'";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_passes_when_related_row_satisfies() {
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();
    apply_and_register(&backend, &mut registry, approval_schema(vec![])).await;
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(REQUIRE_GRANTED, Cardinality::One),
    )
    .await;

    let state = build_state(backend, registry, None).await;
    let app = app_with_claims(state, claims(&["platform_admin"]));

    // A granted approval.
    let (status, approval) = json_request(
        &app,
        Method::POST,
        "/schemas/Approval/entities",
        Some(serde_json::json!({ "fields": { "name": "a", "state": "granted" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "approval create: {approval}");
    let approval_id = approval["id"].as_str().unwrap().to_string();

    // A closed document pointing at the granted approval → @require passes.
    let (status, doc) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed", "approval": approval_id }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "expected 201, got {status}: {doc}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_rejected_when_related_row_fails() {
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();
    apply_and_register(&backend, &mut registry, approval_schema(vec![])).await;
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(REQUIRE_GRANTED, Cardinality::One),
    )
    .await;

    let state = build_state(backend, registry, None).await;
    let app = app_with_claims(state, claims(&["platform_admin"]));

    // A pending (not granted) approval.
    let (status, approval) = json_request(
        &app,
        Method::POST,
        "/schemas/Approval/entities",
        Some(serde_json::json!({ "fields": { "name": "a", "state": "pending" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{approval}");
    let approval_id = approval["id"].as_str().unwrap().to_string();

    // A closed document pointing at the pending approval → @require rejects.
    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed", "approval": approval_id }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422, got {status}: {body}"
    );
    assert!(
        body.to_string().contains("granted approval"),
        "body should carry the require message: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_fails_closed_when_fk_is_null() {
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();
    apply_and_register(&backend, &mut registry, approval_schema(vec![])).await;
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(REQUIRE_GRANTED, Cardinality::One),
    )
    .await;

    let state = build_state(backend, registry, None).await;
    let app = app_with_claims(state, claims(&["platform_admin"]));

    // Closed document with NO approval FK → related.approval binding is absent
    // → fail-closed: the predicate cannot resolve and the write is rejected,
    // never silently allowed.
    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed" }
        })),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::CREATED,
        "a closed doc with no approval must NOT be created: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_fails_closed_when_related_row_missing() {
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();
    apply_and_register(&backend, &mut registry, approval_schema(vec![])).await;
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(REQUIRE_GRANTED, Cardinality::One),
    )
    .await;

    let state = build_state(backend, registry, None).await;
    let app = app_with_claims(state, claims(&["platform_admin"]));

    // Closed document referencing a non-existent approval id → related binding
    // absent → fail-closed (not created).
    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed", "approval": "approval_does_not_exist" }
        })),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::CREATED,
        "a closed doc referencing a missing approval must NOT be created: {body}"
    );

    // The open path still works (the @require disjunction is satisfied by
    // status != 'closed' without ever needing the related row).
    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "draft" }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "draft doc should create: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_tenant_related_row_is_not_readable() {
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();
    apply_and_register(&backend, &mut registry, approval_schema(vec![])).await;
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(REQUIRE_GRANTED, Cardinality::One),
    )
    .await;

    // Tenancy enabled with an Organization root.
    let org = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Organization").unwrap(),
        vec![text_field("name")],
        vec![Annotation::Tenant(TenantKind::Root)],
    )
    .unwrap();
    let tenant_config = TenantConfig::from_schemas(&[org]).unwrap();

    let state = build_state(backend, registry, Some(tenant_config)).await;

    // Tenant A creates a GRANTED approval (stamped _tenant = org-a).
    let app_a = app_with_claims(state.clone(), claims_in_tenant(&["member"], "org-a"));
    let (status, approval) = json_request(
        &app_a,
        Method::POST,
        "/schemas/Approval/entities",
        Some(serde_json::json!({ "fields": { "name": "a", "state": "granted" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{approval}");
    let approval_id = approval["id"].as_str().unwrap().to_string();

    // Tenant B references tenant A's approval id in a closed document. Because
    // the related read is tenant-scoped to org-b, the row is invisible → the
    // related.approval binding is absent → fail-closed (422), proving a rule
    // cannot read across a tenant boundary the caller couldn't otherwise see.
    let app_b = app_with_claims(state, claims_in_tenant(&["member"], "org-b"));
    let (status, body) = json_request(
        &app_b,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed", "approval": approval_id }
        })),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::CREATED,
        "tenant B must NOT read tenant A's approval row through a rule: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn to_many_relation_in_require_is_rejected_at_runtime() {
    // A schema built programmatically can carry a to-many `related.approval`
    // reference (bypassing the DSL apply-time guard); the runtime resolver must
    // reject it rather than mis-resolve. We assert a non-2xx outcome.
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();
    apply_and_register(&backend, &mut registry, approval_schema(vec![])).await;
    // `approval` is to-many here.
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(REQUIRE_GRANTED, Cardinality::Many),
    )
    .await;

    let state = build_state(backend, registry, None).await;
    let app = app_with_claims(state, claims(&["platform_admin"]));

    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed", "approval": ["x"] }
        })),
    )
    .await;
    // A to-many relation is never bound as a single related row, so the
    // `related.approval` reference cannot resolve → fail-closed (not created).
    assert_ne!(
        status,
        StatusCode::CREATED,
        "a to-many related read must not silently pass: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_hop_related_read_is_rejected_with_clear_error() {
    // Approval gains a `reviewer -> Reviewer` relation; Document's @require
    // traverses `related.approval.reviewer.name` — a second relation hop. The
    // runtime resolver must reject this with the multi-hop message.
    let backend = Arc::new(
        SurrealBackend::connect_memory("test", "test")
            .await
            .unwrap(),
    );
    let mut registry = HashMap::new();

    let reviewer = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Reviewer").unwrap(),
        vec![text_field("name")],
        no_access_annotations(),
    )
    .unwrap();
    apply_and_register(&backend, &mut registry, reviewer).await;

    // Approval has a `reviewer` relation.
    apply_and_register(
        &backend,
        &mut registry,
        approval_schema(vec![relation_field(
            "reviewer",
            "Reviewer",
            Cardinality::One,
        )]),
    )
    .await;

    // Document's @require crosses a second relation (approval -> reviewer).
    apply_and_register(
        &backend,
        &mut registry,
        document_schema(
            "status != 'closed' || related.approval.reviewer.name == 'rod'",
            Cardinality::One,
        ),
    )
    .await;

    let state = build_state(backend, registry, None).await;
    let app = app_with_claims(state, claims(&["platform_admin"]));

    // Create an approval first so the FK resolves and the resolver gets far
    // enough to inspect the multi-hop trailing path.
    let (status, approval) = json_request(
        &app,
        Method::POST,
        "/schemas/Approval/entities",
        Some(serde_json::json!({ "fields": { "name": "a", "state": "granted" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{approval}");
    let approval_id = approval["id"].as_str().unwrap().to_string();

    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": { "title": "t", "status": "closed", "approval": approval_id }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "multi-hop read must be a clear rejection, got {status}: {body}"
    );
    assert!(
        body.to_string().contains("multi-hop"),
        "body should mention multi-hop: {body}"
    );
}
