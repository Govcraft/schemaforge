//! Single-record field projection through the complete HTTP read pipeline.
use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    authz::{PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks},
    config::SchemaForgeConfig,
    hooks::{HookBinding, HookOutcome, MockHookDispatcher},
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    storage::StorageRegistry,
    ForgeActor,
};
use schema_forge_backend::{entity::Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::DiffEngine,
    types::{DynamicValue, FieldName, HookEvent},
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::oneshot;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    path: String,
    parent_path: String,
    child_id: String,
    dispatcher: Arc<MockHookDispatcher>,
    _policies: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("projection", "single_get")
            .await
            .unwrap(),
    );
    let mut schema = schema_forge_dsl::parse(
        r#"
        @display("title")
        @hook(before_read) """Gate the read."""
        @hook(after_read) """Decorate the response."""
        @access(read: ["member"], write: ["member"], delete: ["member"])
        schema Run {
            title: text required
            owner: text @owner
            snapshot: text
            secret: text @hidden
            restricted: text @field_access(read: ["admin"], write: ["admin"])
            parent: -> Run
            children: -> Run[]
            read_label: text
        }
    "#,
    )
    .unwrap()
    .remove(0);
    schema
        .fields
        .iter_mut()
        .find(|field| field.name.as_str() == "children")
        .unwrap()
        .derived_from = Some(FieldName::new("parent").unwrap());
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let parent = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Parent".into())),
            ("owner".into(), DynamicValue::Text("alice".into())),
        ]),
    );
    backend.create(&parent).await.unwrap();
    let child = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Child".into())),
            ("owner".into(), DynamicValue::Text("alice".into())),
            (
                "snapshot".into(),
                DynamicValue::Text("x".repeat(1024 * 1024)),
            ),
            ("secret".into(), DynamicValue::Text("hidden secret".into())),
            (
                "restricted".into(),
                DynamicValue::Text("restricted secret".into()),
            ),
            ("parent".into(), DynamicValue::Ref(parent.id.clone())),
        ]),
    );
    backend.create(&child).await.unwrap();
    let dispatcher = Arc::new(MockHookDispatcher::new());
    dispatcher
        .respond_before(
            "Run",
            HookEvent::AfterRead,
            HookOutcome {
                modified_fields: Some(BTreeMap::from([(
                    "read_label".into(),
                    DynamicValue::Text("decorated".into()),
                )])),
                ..Default::default()
            },
        )
        .await;
    let mut config = Config::<SchemaForgeConfig>::default();
    config.custom.schema_forge.hooks.enabled = true;
    config.custom.schema_forge.hooks.bindings = [HookEvent::BeforeRead, HookEvent::AfterRead]
        .into_iter()
        .map(|event| HookBinding {
            schema: "Run".into(),
            event,
            endpoint: "http://test-hook.invalid".into(),
            timeout_ms: None,
            required: true,
            descriptor_path: None,
        })
        .collect();
    let policies = tempfile::tempdir().unwrap();
    std::fs::write(
        policies.path().join("owner-read.cedar"),
        r#"forbid(principal, action == Action::"ReadRun", resource is Run)
        when { !context.resource_is_placeholder && resource has owner && resource.owner != principal.id };"#,
    )
    .unwrap();
    let policy_store = Arc::new(PolicyStore::new(
        PolicyStoreSnapshot::from_schemas(
            std::slice::from_ref(&schema),
            Some(policies.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap(),
    ));
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .build();
    let (tx, rx) = oneshot::channel();
    service
        .state()
        .actor::<ForgeActor>()
        .unwrap()
        .send(InitForge {
            registry: [(schema.name.to_string(), schema)].into(),
            backend,
            tenant_config: None,
            record_access_policy: None,
            hook_dispatcher: Some(dispatcher.clone()),
            storage_registry: StorageRegistry::default(),
            policy_store: Some(policy_store),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    let app = forge_routes()
        .layer(axum::middleware::from_fn(
            |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
                let sub = req
                    .headers()
                    .get("x-test-subject")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("user:alice")
                    .to_owned();
                let role = if req.headers().contains_key("x-test-denied") {
                    "unrelated"
                } else {
                    "member"
                };
                req.extensions_mut().insert(Claims {
                    sub,
                    roles: vec![role.into()],
                    perms: vec![],
                    exp: 9_999_999_999,
                    iat: None,
                    jti: None,
                    iss: None,
                    aud: None,
                    email: None,
                    username: None,
                    custom: Default::default(),
                });
                next.run(req).await
            },
        ))
        .with_state(service.state().clone());
    Fixture {
        app,
        path: format!("/schemas/Run/entities/{}", child.id),
        parent_path: format!("/schemas/Run/entities/{}", parent.id),
        child_id: child.id.to_string(),
        dispatcher,
        _policies: policies,
    }
}

async fn get(
    app: &Router,
    path: &str,
    extra_header: Option<(&str, &str)>,
) -> (StatusCode, HeaderMap, serde_json::Value) {
    let mut request = Request::builder().uri(path);
    if let Some((name, value)) = extra_header {
        request = request.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_get_projects_after_full_authorization_and_read_hooks() {
    let f = fixture().await;
    let (status, headers, full) = get(&f.app, &f.path, None).await;
    assert_eq!(status, StatusCode::OK, "{full}");
    assert_eq!(
        full["fields"]["snapshot"].as_str().unwrap().len(),
        1024 * 1024
    );
    assert_eq!(full["fields"]["parent__display"], "Parent");
    assert_eq!(full["fields"]["read_label"], "decorated");
    let (status, projected_headers, projected) = get(
        &f.app,
        &format!("{}?fields=title,title,read_label,secret,restricted", f.path),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{projected}");
    assert!(
        projected["fields"].get("snapshot").is_none(),
        "projection must omit the large snapshot"
    );
    assert_eq!(
        projected["fields"],
        serde_json::json!({"title":"Child", "read_label":"decorated"})
    );
    assert_eq!(projected["id"], full["id"]);
    assert_eq!(projected["schema"], full["schema"]);
    assert_eq!(projected["permissions"], full["permissions"]);
    assert_eq!(
        headers.get("entity-revision"),
        projected_headers.get("entity-revision")
    );
    let calls = f.dispatcher.before_calls().await;
    let after_read = calls
        .iter()
        .rev()
        .find(|call| call.event == HookEvent::AfterRead)
        .unwrap();
    assert!(after_read.fields.contains_key("owner"));
    assert!(after_read.fields.contains_key("snapshot"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_get_projection_validates_names_and_preserves_access_denial() {
    let f = fixture().await;
    for fields in ["missing", "title,missing", "", ",,"] {
        let (status, _, body) = get(&f.app, &format!("{}?fields={fields}", f.path), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "fields={fields}");
        assert_eq!(body["error"], "invalid_query");
    }
    for header in [("x-test-subject", "user:bob"), ("x-test-denied", "1")] {
        let (status, _, body) =
            get(&f.app, &format!("{}?fields=title", f.path), Some(header)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let missing = schema_forge_core::types::EntityId::new("run");
    let (status, _, _) = get(
        &f.app,
        &format!("/schemas/Run/entities/{missing}?fields=title"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_get_projection_retains_only_selected_relation_companions_and_derived_fields() {
    let f = fixture().await;
    let (status, _, body) = get(&f.app, &format!("{}?fields=parent", f.path), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"].as_object().unwrap().len(), 2);
    assert_eq!(body["fields"]["parent__display"], "Parent");
    let (status, _, unresolved) = get(
        &f.app,
        &format!("{}?fields=parent&resolve=false", f.path),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unresolved}");
    assert_eq!(
        unresolved["fields"],
        serde_json::json!({"parent":body["fields"]["parent"]})
    );
    let (status, _, derived) =
        get(&f.app, &format!("{}?fields=children", f.parent_path), None).await;
    assert_eq!(status, StatusCode::OK, "{derived}");
    assert_eq!(
        derived["fields"],
        serde_json::json!({"children":[f.child_id], "children__display":["Child"]})
    );
}
