//! Backend-neutral authenticated SSE fixtures and committed CRUD projection assertions.
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
    events::{ConfigureEvents, EventsRuntime},
    events_config::EventsConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    storage::StorageRegistry,
    DynAuthStore, ForgeActor, SchemaForgeConfig,
};
use schema_forge_backend::{AuthStore, EntityAuthStore, EntityStore, SchemaBackend};
use schema_forge_core::{migration::DiffEngine, system_schemas};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::oneshot;
use tower::ServiceExt;

pub struct Fixture {
    pub app: Router,
    pub anonymous: Router,
    pub store: Arc<dyn DynAuthStore>,
    pub membership: Option<String>,
    pub other_tenant: Option<String>,
}
pub async fn fixture_backend<B: SchemaBackend + EntityStore + 'static>(
    backend: Arc<B>,
    config: EventsConfig,
) -> Fixture {
    fixture_options(backend, config, false, false).await
}
pub async fn fixture_options<B: SchemaBackend + EntityStore + 'static>(
    backend: Arc<B>,
    config: EventsConfig,
    tenancy: bool,
    read_hooks: bool,
) -> Fixture {
    fixture_identity_options(backend, config, tenancy, read_hooks, false).await
}

pub async fn fixture_identity_options<B: SchemaBackend + EntityStore + 'static>(
    backend: Arc<B>,
    config: EventsConfig,
    tenancy: bool,
    read_hooks: bool,
    scoped_role: bool,
) -> Fixture {
    let source = format!(
        "{}{}",
        if tenancy {
            "@tenant(parent: \"Org\") "
        } else {
            ""
        },
        r#"
        @access(read: ["member"], write: ["member"], delete: ["member"])
        schema Note {
            title: text required
            category: text
            secret: text @hidden @compute("'stored-secret'")
            restricted: text @field_access(read: ["admin"], write: ["member"])
            owner: text @owner
            parent: -> Note
            children: -> Note[]
            read_label: text
        }
    "#
    );
    let source = if read_hooks {
        format!(
            r#"@hook(before_read) """Authorize read decoration.""" @hook(after_read) """Decorate this read.""" {source}"#
        )
    } else {
        source
    };
    let mut note = schema_forge_dsl::parse(&source).unwrap().remove(0);
    note.fields
        .iter_mut()
        .find(|field| field.name.as_str() == "children")
        .unwrap()
        .derived_from = Some(schema_forge_core::types::FieldName::new("parent").unwrap());
    let user = schema_forge_dsl::parse(system_schemas::USER_SCHEMA)
        .unwrap()
        .remove(0);
    let mut schemas = vec![note.clone(), user.clone()];
    let tm = schema_forge_dsl::parse(system_schemas::TENANT_MEMBERSHIP_SCHEMA)
        .unwrap()
        .remove(0);
    if tenancy {
        schemas.push(tm.clone());
        schemas.push(
            schema_forge_dsl::parse("@tenant(root) schema Org { name: text required }")
                .unwrap()
                .remove(0),
        );
    }
    let tenant_config = if tenancy {
        Some(schema_forge_backend::TenantConfig::from_schemas(&schemas).unwrap())
    } else {
        None
    };
    for schema in &schemas {
        backend
            .apply_migration(&schema.name, &DiffEngine::create_new(schema).steps)
            .await
            .unwrap();
        backend.store_schema_metadata(schema).await.unwrap();
    }
    backend.finalize_schema_migrations().await.unwrap();
    let mut store = EntityAuthStore::new(backend.clone(), user.clone(), Arc::new(|_| Some(10)));
    if tenancy {
        store = store.with_tenant_membership_schema(tm.clone());
    }
    let store = Arc::new(store);
    let global_roles = if scoped_role {
        Vec::new()
    } else {
        vec!["member".into()]
    };
    AuthStore::create_user_without_password(store.as_ref(), "alice", &global_roles, "Alice")
        .await
        .unwrap();
    let mut chain = Vec::<schema_forge_backend::TenantRef>::new();
    let mut other_tenant = None;
    let mut membership = None;
    if tenancy {
        for name in ["Mine", "Other"] {
            let entity = schema_forge_backend::Entity::new(
                schema_forge_core::types::SchemaName::new("Org").unwrap(),
                std::collections::BTreeMap::from([(
                    "name".into(),
                    schema_forge_core::types::DynamicValue::Text(name.into()),
                )]),
            );
            backend.create(&entity).await.unwrap();
            if name == "Mine" {
                AuthStore::add_tenant_membership(
                    store.as_ref(),
                    "alice",
                    "Org",
                    entity.id.as_str(),
                    Some("member"),
                )
                .await
                .unwrap();
                chain.push(schema_forge_backend::TenantRef {
                    schema: "Org".into(),
                    entity_id: entity.id.to_string(),
                });
            } else {
                other_tenant = Some(entity.id.to_string());
            }
        }
        let rows = backend
            .query(&schema_forge_core::query::Query::new(tm.id.clone()))
            .await
            .unwrap();
        membership = Some(rows.entities[0].id.to_string());
    }
    // @owner protects mutations; this operator policy independently restricts reads.
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("owner-read.cedar"), r#"
        forbid (principal is Forge::Principal, action == Action::"ReadNote", resource is Note)
        when { resource has owner && resource.owner != principal.id && !(principal in Forge::Group::"platform_admin") };
    "#).unwrap();
    let policy_store = Arc::new(schema_forge_acton::authz::PolicyStore::new(
        schema_forge_acton::authz::PolicyStoreSnapshot::from_schemas(
            &schemas,
            Some(directory.path()),
            schema_forge_acton::authz::RoleRanks::empty(),
            schema_forge_acton::authz::PrincipalClaimMappings::default(),
        )
        .unwrap(),
    ));
    let entity_store: Arc<dyn schema_forge_backend::DynEntityStore> = backend.clone();
    let auth_store: Arc<dyn DynAuthStore> = store;
    let mut service_config = Config::<SchemaForgeConfig>::default();
    let dispatcher = Arc::new(schema_forge_acton::hooks::MockHookDispatcher::new());
    if read_hooks {
        use schema_forge_acton::hooks::{HookBinding, HookOutcome};
        use schema_forge_core::types::HookEvent;
        service_config.custom.schema_forge.hooks.enabled = true;
        service_config.custom.schema_forge.hooks.bindings =
            [HookEvent::BeforeRead, HookEvent::AfterRead]
                .into_iter()
                .map(|event| HookBinding {
                    schema: "Note".into(),
                    event,
                    endpoint: "http://test-hook.invalid".into(),
                    timeout_ms: None,
                    required: true,
                    descriptor_path: None,
                })
                .collect();
        dispatcher
            .respond_before(
                "Note",
                HookEvent::AfterRead,
                HookOutcome {
                    modified_fields: Some(std::collections::BTreeMap::from([(
                        "read_label".into(),
                        schema_forge_core::types::DynamicValue::Text("decorated".into()),
                    )])),
                    ..Default::default()
                },
            )
            .await;
    }
    let service = ServiceBuilder::new()
        .with_config(service_config)
        .with_actor::<ForgeActor>()
        .build();
    let handle = service.state().actor::<ForgeActor>().unwrap();
    let (tx, rx) = oneshot::channel();
    handle
        .send(InitForge {
            registry: schemas
                .into_iter()
                .map(|s| (s.name.to_string(), s))
                .collect(),
            backend: backend.clone(),
            tenant_config: tenant_config.clone(),
            record_access_policy: None,
            hook_dispatcher: read_hooks.then_some(dispatcher),
            storage_registry: StorageRegistry::default(),
            policy_store: Some(policy_store),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    if config.enabled {
        let (tx, rx) = oneshot::channel();
        handle
            .send(ConfigureEvents {
                runtime: Arc::new(EventsRuntime::new(config, auth_store.clone(), backend).unwrap()),
                reply: ReplyChannel::new(tx),
            })
            .await;
        rx.await.unwrap();
    }
    let anonymous = forge_routes().with_state(service.state().clone());
    // Login mints `user:<username>`; the auth store is keyed by the bare name.
    let caller = Claims {
        sub: "user:alice".into(),
        roles: global_roles,
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: if tenancy {
            HashMap::from([
                ("tenant_chain".into(), serde_json::to_value(chain).unwrap()),
                (
                    "tenant_roles".into(),
                    serde_json::to_value(auth_store.list_tenant_roles("alice").await.unwrap())
                        .unwrap(),
                ),
            ])
        } else {
            HashMap::new()
        },
    };
    let scoped = if tenancy {
        forge_routes().layer(axum::middleware::from_fn_with_state(
            schema_forge_acton::middleware::tenant_scope::TenantScopeState {
                entity_store,
                tenant_config: Arc::new(tenant_config),
            },
            schema_forge_acton::middleware::tenant_scope::middleware,
        ))
    } else {
        forge_routes()
    };
    let app = scoped
        .layer(axum::middleware::from_fn(
            move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                let mut claims = caller.clone();
                if request.headers().contains_key("x-test-admin") {
                    claims.roles = vec!["platform_admin".into()];
                }
                if let Some(subject) = request.headers().get("x-test-subject") {
                    claims.sub = subject.to_str().unwrap().into();
                }
                async move {
                    request.extensions_mut().insert(claims);
                    next.run(request).await
                }
            },
        ))
        .with_state(service.state().clone());
    Fixture {
        app,
        anonymous,
        store: auth_store,
        membership,
        other_tenant,
    }
}
pub fn enabled() -> EventsConfig {
    EventsConfig {
        enabled: true,
        keep_alive_secs: 1,
        ..Default::default()
    }
}
pub async fn request(
    app: &Router,
    method: &str,
    path: &str,
    fields: serde_json::Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"fields":fields}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}
pub async fn json(response: axum::response::Response) -> serde_json::Value {
    assert!(
        response.status().is_success(),
        "response status: {}",
        response.status()
    );
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
pub async fn connect(app: &Router, query: &str) -> Body {
    let response = request(
        app,
        "GET",
        &format!("/schemas/Note/events{query}"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let mut body = response.into_body();
    assert!(frame(&mut body).await.contains("retry:"));
    body
}
pub async fn frame(body: &mut Body) -> String {
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("SSE deadline")
        .expect("SSE ended")
        .unwrap();
    String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
}
pub async fn change(body: &mut Body) -> (String, serde_json::Value) {
    loop {
        let frame = frame(body).await;
        if let Some(data) = frame.lines().find_map(|line| {
            line.strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
        }) {
            return (frame.clone(), serde_json::from_str(data).unwrap());
        }
    }
}
pub async fn exercise_crud(f: &Fixture) {
    let mut body = connect(&f.app, "").await;
    let created = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"First", "category":"books", "restricted":"private"}),
        )
        .await,
    )
    .await;
    let path = format!("/schemas/Note/entities/{}", created["id"].as_str().unwrap());
    let detail_response = request(
        &f.app,
        "GET",
        &format!("{path}?resolve=false"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let bytes = detail_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let detail_json = std::str::from_utf8(&bytes).unwrap();
    let detail: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let (frame, event) = change(&mut body).await;
    assert!(frame.contains("event: entity.created"));
    assert!(frame.contains(event["event_id"].as_str().unwrap()));
    assert_eq!(event["actor"], "user:alice");
    assert_eq!(event["entity"], detail);
    assert!(
        frame.contains(&format!("\"entity\":{detail_json}")),
        "SSE entity must preserve GET serialization"
    );
    assert!(event["entity"]["fields"].get("secret").is_none());
    assert!(event["entity"]["fields"].get("restricted").is_none());
    for method in ["PUT", "PATCH"] {
        let changed = json(
            request(
                &f.app,
                method,
                &path,
                serde_json::json!({"title":method, "category":"books"}),
            )
            .await,
        )
        .await;
        assert_eq!(changed["fields"]["title"], method);
        let (_, event) = change(&mut body).await;
        assert_eq!(event["event_type"], "entity.updated");
        let detail = json(
            request(
                &f.app,
                "GET",
                &format!("{path}?resolve=false"),
                serde_json::Value::Null,
            )
            .await,
        )
        .await;
        assert_eq!(event["entity"], detail);
    }
    assert_eq!(
        request(&f.app, "DELETE", &path, serde_json::Value::Null)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let (_, event) = change(&mut body).await;
    assert_eq!(event["event_type"], "entity.deleted");
    assert_eq!(event["entity_id"], created["id"]);
    assert!(event.get("entity").is_none());
    assert!(!event.to_string().contains("stored-secret"));
}
