//! Backend-neutral create-intent HTTP fixtures and tenant-aware requests.
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
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;
use tower::ServiceExt;

pub async fn fixture_with_backend(
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

pub async fn request(
    app: &Router,
    path: &str,
    method: &str,
    condition: Option<&str>,
    fields: serde_json::Value,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    request_in_tenant(app, path, method, condition, fields, None).await
}

pub async fn request_in_tenant(
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
