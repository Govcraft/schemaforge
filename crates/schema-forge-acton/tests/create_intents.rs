//! HTTP record revision contracts for unsupported and explicitly disposable
//! PostgreSQL storage, including authorization before token/capability errors.
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
    state::{DynEntityStore, DynForgeBackend},
    storage::StorageRegistry,
    ForgeActor,
};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType,
    SchemaDefinition, SchemaId, SchemaName, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;
use tower::ServiceExt;

async fn fixture(owner: &str, roles: &[&str]) -> (Router, String) {
    let backend = Arc::new(
        SurrealBackend::connect_memory("conditional", "conditional")
            .await
            .unwrap(),
    );
    fixture_with_backend(backend, owner, roles, false).await
}

async fn fixture_with_backend(
    backend: Arc<dyn DynForgeBackend>,
    owner: &str,
    roles: &[&str],
    prepare_revisions: bool,
) -> (Router, String) {
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Note").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_annotations(
                FieldName::new("owner").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::Owner],
            ),
            FieldDefinition::with_annotations(
                FieldName::new("restricted").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::FieldAccess {
                    read: vec!["editor".into()],
                    write: vec!["manager".into()],
                }],
            ),
        ],
        vec![Annotation::Access {
            read: vec!["editor".into()],
            write: vec!["editor".into()],
            delete: vec!["editor".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    if prepare_revisions {
        backend
            .prepare_record_revisions(&schema.name)
            .await
            .unwrap();
    }
    let entity = Entity::with_id(
        EntityId::new("note"),
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Original".into())),
            ("owner".into(), DynamicValue::Text(owner.into())),
            ("restricted".into(), DynamicValue::Text("Restricted".into())),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &entity)
        .await
        .unwrap();
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
            registry: HashMap::from([("Note".into(), schema)]),
            backend,
            tenant_config: None,
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
    let caller = Claims {
        sub: "user:editor".into(),
        roles: roles.iter().map(|role| (*role).into()).collect(),
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    };
    let app = forge_routes()
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let caller = caller.clone();
                async move {
                    req.extensions_mut().insert(caller);
                    next.run(req).await
                }
            },
        ))
        .with_state(service.state().clone());
    (app, format!("/schemas/Note/entities/{}", entity.id))
}

async fn request(
    app: &Router,
    path: &str,
    method: &str,
    condition: Option<&str>,
    fields: serde_json::Value,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    request_in_tenant(app, path, method, condition, fields, None).await
}

async fn request_in_tenant(
    app: &Router,
    path: &str,
    method: &str,
    condition: Option<&str>,
    fields: serde_json::Value,
    tenant: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(condition) = condition {
        request = request.header("create-intent", condition);
    }
    if let Some(tenant) = tenant {
        request = request.header("x-active-tenant", tenant);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::json!({"fields": fields}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_intents_fail_closed_for_unsupported_and_denied_input() {
    let (app, _) = fixture("editor", &["editor"]).await;
    let (status, _, body) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        serde_json::json!({"title":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "create_intent_unsupported");
    let (status, _, body) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        serde_json::json!({"restricted":"denied"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (app, _) = fixture("editor", &["other"]).await;
    let (status, _, body) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        serde_json::json!({"title":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a scoped disposable PostgreSQL URL and SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1"]
async fn postgres_http_create_reconciliation() {
    assert_eq!(
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE").as_deref(),
        Ok("1")
    );
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").unwrap();
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("disposable test database connection failed")),
    );
    let (app, _) = fixture_with_backend(backend.clone(), "editor", &["editor"], true).await;
    let fields = serde_json::json!({"title":"New record"});
    let (status, _, receipt) = request(
        &app,
        "/schemas/Note/create-intents",
        "POST",
        None,
        fields.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    assert_eq!(receipt["state"], "pending");
    let id = receipt["id"].as_str().unwrap();
    let receipt_path = format!("/schemas/Note/create-intents/{id}");
    let (status, _, body) = request(&app, &receipt_path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "pending");
    let (status, headers, entity) = request(
        &app,
        "/schemas/Note/entities",
        "POST",
        Some(id),
        fields.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{entity}");
    assert_eq!(headers["create-intent"], id);
    assert!(headers.contains_key("entity-revision"));
    let original_revision = headers["entity-revision"].clone();
    let (status, headers, replay) = request(
        &app,
        "/schemas/Note/entities",
        "POST",
        Some(id),
        fields.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(entity, replay);
    assert_eq!(headers["entity-revision"], original_revision);
    let (status, _, body) = request(
        &app,
        "/schemas/Note/entities",
        "POST",
        Some(id),
        serde_json::json!({"title":"changed"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "create_intent_content_conflict");
    let entity_id = EntityId::parse(entity["id"].as_str().unwrap()).unwrap();
    let change = Entity::with_id(
        entity_id.clone(),
        SchemaName::new("Note").unwrap(),
        BTreeMap::from([("owner".into(), DynamicValue::Text("other".into()))]),
    );
    DynEntityStore::update(backend.as_ref(), &change)
        .await
        .unwrap();
    let (status, _, body) = request(&app, &receipt_path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _, body) = request(
        &app,
        "/schemas/Note/entities",
        "POST",
        Some(id),
        fields.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    DynEntityStore::delete(backend.as_ref(), &change.schema, &entity_id)
        .await
        .unwrap();
    let (status, _, body) = request(&app, &receipt_path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "committed_unavailable");
    assert!(body["entity_id"].is_null());
    let (status, _, body) = request(&app, "/schemas/Note/entities", "POST", Some(id), fields).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "create_result_unavailable");
}

#[cfg(feature = "postgres")]
#[ignore = "requires a scoped disposable PostgreSQL namespace"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_create_intents_honor_selected_membership() {
    use schema_forge_acton::middleware::tenant_scope::{middleware, TenantScopeState};

    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("TenantNote").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("title").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![
            Annotation::Access {
                read: vec!["editor".into()],
                write: vec!["editor".into()],
                delete: vec!["editor".into()],
                cross_tenant_read: vec![],
            },
            Annotation::Tenant(schema_forge_core::types::TenantKind::Child {
                parent: SchemaName::new("Organization").unwrap(),
            }),
        ],
    )
    .unwrap();
    let organization = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Organization").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Tenant(
            schema_forge_core::types::TenantKind::Root,
        )],
    )
    .unwrap();
    assert_eq!(
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE").as_deref(),
        Ok("1")
    );
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").unwrap();
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("disposable test database connection failed")),
    );
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    schema_forge_acton::DynSchemaBackend::apply_migration(
        backend.as_ref(),
        &schema.name,
        &plan.steps,
    )
    .await
    .unwrap();
    schema_forge_acton::DynSchemaBackend::store_schema_metadata(backend.as_ref(), &schema)
        .await
        .unwrap();
    let entity = Entity::with_id(
        EntityId::new("tenantnote"),
        schema.name.clone(),
        BTreeMap::from([
            (
                "title".into(),
                DynamicValue::Text("Program alpha record".into()),
            ),
            (
                "_tenant".into(),
                DynamicValue::Text("organization_alpha".into()),
            ),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &entity)
        .await
        .unwrap();
    let tenant_config = schema_forge_backend::tenant::TenantConfig::from_schemas(&[
        organization.clone(),
        schema.clone(),
    ])
    .unwrap();
    let scope = TenantScopeState {
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
                ("TenantNote".into(), schema),
                ("Organization".into(), organization),
            ]),
            backend: backend.clone(),
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
    let caller = Claims {
        sub: "user:editor".into(),
        roles: vec!["editor".into()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::from([(
            "tenant_chain".into(),
            serde_json::json!([
                {"schema":"Organization","entity_id":"organization_alpha"},
                {"schema":"Organization","entity_id":"organization_beta"},
            ]),
        )]),
    };
    let app = forge_routes()
        .layer(axum::middleware::from_fn_with_state(scope, middleware))
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let caller = caller.clone();
                async move {
                    req.extensions_mut().insert(caller);
                    next.run(req).await
                }
            },
        ))
        .with_state(service.state().clone());
    let alpha = Some("Organization:organization_alpha");
    let beta = Some("Organization:organization_beta");
    let fields = serde_json::json!({"title":"Scoped create"});
    let (status, _, receipt) = request_in_tenant(
        &app,
        "/schemas/TenantNote/create-intents",
        "POST",
        None,
        fields.clone(),
        alpha,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    let id = receipt["id"].as_str().unwrap();
    let receipt_path = format!("/schemas/TenantNote/create-intents/{id}");
    for (path, method) in [
        (&receipt_path[..], "GET"),
        ("/schemas/TenantNote/entities", "POST"),
    ] {
        let (status, _, body) =
            request_in_tenant(&app, path, method, Some(id), fields.clone(), beta).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["reason"], "create_intent_unavailable");
    }
    for tenant in [None, Some("Organization:organization_unknown")] {
        let (status, _, body) = request_in_tenant(
            &app,
            "/schemas/TenantNote/entities",
            "POST",
            Some(id),
            fields.clone(),
            tenant,
        )
        .await;
        assert!(status.is_client_error(), "{body}");
    }
    let (status, _, entity) = request_in_tenant(
        &app,
        "/schemas/TenantNote/entities",
        "POST",
        Some(id),
        fields.clone(),
        alpha,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{entity}");
    let (status, _, body) = request_in_tenant(
        &app,
        &receipt_path,
        "GET",
        None,
        serde_json::json!({}),
        beta,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "create_intent_unavailable");
    let (status, _, body) = request_in_tenant(
        &app,
        &receipt_path,
        "GET",
        None,
        serde_json::json!({}),
        alpha,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "committed");
}
