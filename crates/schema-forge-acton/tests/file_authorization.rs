//! File routes must honor the same record and field boundary as entity access.
//! No object store is configured: an authorized request reaches storage lookup,
//! while a denied request must stop with 403 before any storage work.
use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    state::DynEntityStore,
    storage::StorageRegistry,
    DynSchemaBackend, ForgeActor,
};
use schema_forge_backend::{entity::Entity, tenant::TenantConfig};
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType,
    FileAccess, FileConstraints, MimePattern, SchemaDefinition, SchemaId, SchemaName, TenantKind,
    TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;
use tower::ServiceExt;

fn claims(tenant: Option<&str>, roles: &[&str]) -> Claims {
    let mut custom = HashMap::new();
    if let Some(tenant) = tenant {
        custom.insert(
            "tenant_chain".into(),
            serde_json::json!([{"schema":"Organization","entity_id":tenant}]),
        );
    }
    Claims {
        sub: "user:file-tester".into(),
        roles: roles.iter().map(|r| (*r).into()).collect(),
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom,
    }
}

async fn fixture(caller: Option<Claims>, restricted: bool) -> (Router, String) {
    fixture_with_owner(caller, restricted, None).await
}

async fn fixture_with_owner(
    caller: Option<Claims>,
    restricted: bool,
    owner: Option<&str>,
) -> (Router, String) {
    let field_annotations = if restricted {
        vec![FieldAnnotation::FieldAccess {
            read: vec!["file_reader".into()],
            write: vec!["file_writer".into()],
        }]
    } else {
        vec![]
    };
    let mut definitions = vec![FieldDefinition::with_annotations(
        FieldName::new("attachment").unwrap(),
        FieldType::File(FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 1024,
            mime_allowlist: vec![MimePattern::Exact("text/plain".into())],
            access: FileAccess::Presigned,
        }),
        vec![],
        field_annotations,
    )];
    if owner.is_some() {
        definitions.push(FieldDefinition::with_annotations(
            FieldName::new("owner").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::Owner],
        ));
    }
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Document").unwrap(),
        definitions,
        vec![
            Annotation::Access {
                read: vec!["editor".into()],
                write: vec!["editor".into()],
                delete: vec![],
                cross_tenant_read: vec![],
            },
            Annotation::Tenant(TenantKind::Child {
                parent: SchemaName::new("Organization").unwrap(),
            }),
        ],
    )
    .unwrap();
    let backend = Arc::new(
        SurrealBackend::connect_memory("files", "files")
            .await
            .unwrap(),
    );
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let mut fields = BTreeMap::new();
    if let Some(owner) = owner {
        fields.insert("owner".into(), DynamicValue::Text(owner.into()));
    }
    fields.insert(
        "_tenant".into(),
        DynamicValue::Text("organization_alpha".into()),
    );
    fields.insert("attachment".into(), DynamicValue::Json(serde_json::json!({"key":"synthetic/result.txt","size":1,"mime":"text/plain","status":"available","created_at":"2026-01-01T00:00:00Z","uploaded_at":"2026-01-01T00:00:00Z","checksum":null})));
    let entity = Entity::with_id(EntityId::new("document"), schema.name.clone(), fields);
    DynEntityStore::create(backend.as_ref(), &entity)
        .await
        .unwrap();
    let organization = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Organization").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Tenant(TenantKind::Root)],
    )
    .unwrap();
    let tenant_config =
        TenantConfig::from_schemas(&[organization.clone(), schema.clone()]).unwrap();
    let tenant_scope_state = schema_forge_acton::middleware::tenant_scope::TenantScopeState {
        entity_store: backend.clone(),
        tenant_config: Arc::new(Some(tenant_config.clone())),
    };
    let service = ServiceBuilder::new()
        .with_config(Config::<SchemaForgeConfig>::default())
        .with_actor::<ForgeActor>()
        .build();
    let (tx, rx) = oneshot::channel();
    service
        .state()
        .actor::<ForgeActor>()
        .unwrap()
        .send(InitForge {
            registry: HashMap::from([
                ("Document".into(), schema),
                ("Organization".into(), organization),
            ]),
            backend,
            tenant_config: Some(tenant_config),
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: StorageRegistry::default(),
            policy_store: None,
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    let app = forge_routes()
        .layer(axum::middleware::from_fn_with_state(
            tenant_scope_state,
            schema_forge_acton::middleware::tenant_scope::middleware,
        ))
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let caller = caller.clone();
                async move {
                    if let Some(caller) = caller {
                        req.extensions_mut().insert(caller);
                    }
                    next.run(req).await
                }
            },
        ))
        .with_state(service.state().clone());
    (
        app,
        format!("/schemas/Document/entities/{}/fields/attachment", entity.id),
    )
}

async fn status(app: &Router, path: &str, operation: &str) -> StatusCode {
    status_with_tenant(app, path, operation, None).await
}

async fn status_with_tenant(
    app: &Router,
    path: &str,
    operation: &str,
    tenant: Option<&str>,
) -> StatusCode {
    let (method, suffix, body) = match operation {
        "download" => ("GET", "?redirect=false", ""),
        "mint" => (
            "POST",
            "/upload-url",
            r#"{"filename":"result.txt","mime":"text/plain","size":1}"#,
        ),
        "confirm" => (
            "POST",
            "/confirm-upload",
            r#"{"key":"organization_alpha/Document/placeholder/attachment/result.txt"}"#,
        ),
        _ => ("POST", "/scan-complete", r#"{"status":"available"}"#),
    };
    let mut request = Request::builder()
        .method(method)
        .uri(format!("{path}{suffix}"))
        .header("content-type", "application/json");
    if let Some(tenant) = tenant {
        request = request.header("x-active-tenant", tenant);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            body.contains("storage backend 'documents' not configured"),
            "unexpected internal error: {body}"
        );
    }
    status
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_routes_deny_another_tenant_and_missing_tenant_context() {
    for tenant in [Some("organization_beta"), None] {
        let (app, path) = fixture(Some(claims(tenant, &["editor"])), false).await;
        let entity_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path.trim_end_matches("/fields/attachment"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(entity_response.status(), StatusCode::FORBIDDEN);
        for operation in ["download", "mint", "confirm", "scan"] {
            assert_eq!(
                status(&app, &path, operation).await,
                StatusCode::FORBIDDEN,
                "{tenant:?} {operation}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_routes_require_authentication() {
    let (app, path) = fixture(None, false).await;
    for operation in ["download", "mint", "confirm", "scan"] {
        assert_eq!(
            status(&app, &path, operation).await,
            StatusCode::UNAUTHORIZED,
            "{operation}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_tenant_and_platform_admin_reach_storage() {
    for caller in [
        claims(Some("organization_alpha"), &["editor"]),
        claims(None, &["platform_admin"]),
    ] {
        let admin = caller.roles.iter().any(|role| role == "platform_admin");
        let (app, path) = fixture(Some(caller), false).await;
        assert_eq!(
            status(&app, &path, "confirm").await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            status(&app, &path, "scan").await,
            if admin {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::FORBIDDEN
            }
        );
        for operation in ["download", "mint"] {
            assert_eq!(
                status(&app, &path, operation).await,
                StatusCode::INTERNAL_SERVER_ERROR,
                "authorized {operation} must reach missing storage backend"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_file_field_denies_otherwise_authorized_record() {
    let (app, path) = fixture(Some(claims(Some("organization_alpha"), &["editor"])), true).await;
    for operation in ["download", "mint", "confirm"] {
        assert_eq!(
            status(&app, &path, operation).await,
            StatusCode::FORBIDDEN,
            "{operation}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_admin_can_access_restricted_fields_without_tenant() {
    let (app, path) = fixture(Some(claims(None, &["platform_admin"])), true).await;
    for operation in ["download", "mint"] {
        assert_eq!(
            status(&app, &path, operation).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn field_permissions_separate_read_and_write() {
    let (reader, path) = fixture(
        Some(claims(
            Some("organization_alpha"),
            &["editor", "file_reader"],
        )),
        true,
    )
    .await;
    assert_eq!(
        status(&reader, &path, "download").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(status(&reader, &path, "mint").await, StatusCode::FORBIDDEN);
    let (writer, path) = fixture(
        Some(claims(
            Some("organization_alpha"),
            &["editor", "file_writer"],
        )),
        true,
    )
    .await;
    assert_eq!(
        status(&writer, &path, "download").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(&writer, &path, "mint").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_tenant_cannot_modify_another_owners_file() {
    let caller = Some(claims(Some("organization_alpha"), &["editor"]));
    let (app, path) = fixture_with_owner(caller.clone(), false, Some("someone-else")).await;
    for operation in ["mint", "confirm"] {
        assert_eq!(status(&app, &path, operation).await, StatusCode::FORBIDDEN);
    }
    let (app, path) = fixture_with_owner(caller, false, Some("file-tester")).await;
    assert_eq!(
        status(&app, &path, "mint").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_tenant_scopes_files_for_multi_membership_callers() {
    let mut caller = claims(None, &["editor"]);
    caller.custom.insert(
        "tenant_chain".into(),
        serde_json::json!([
            {"schema":"Organization","entity_id":"organization_alpha"},
            {"schema":"Organization","entity_id":"organization_beta"}
        ]),
    );
    let (app, path) = fixture(Some(caller), false).await;
    for (tenant, expected_entity) in [
        (Some("Organization:organization_alpha"), StatusCode::OK),
        (
            Some("Organization:organization_beta"),
            StatusCode::FORBIDDEN,
        ),
        (None, StatusCode::BAD_REQUEST),
        (
            Some("Organization:organization_gamma"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = Request::builder().uri(path.trim_end_matches("/fields/attachment"));
        if let Some(tenant) = tenant {
            request = request.header("x-active-tenant", tenant);
        }
        let entity_response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            entity_response.status(),
            expected_entity,
            "entity {tenant:?}"
        );
        for operation in ["download", "mint", "confirm"] {
            let expected = if expected_entity == StatusCode::OK {
                if operation == "confirm" {
                    StatusCode::UNPROCESSABLE_ENTITY
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            } else {
                expected_entity
            };
            assert_eq!(
                status_with_tenant(&app, &path, operation, tenant).await,
                expected,
                "{operation} {tenant:?}"
            );
        }
    }
}
