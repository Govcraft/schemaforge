//! Actual HTTP streams, actor commits, durable identity and backend projection.
#![cfg(feature = "sse")]
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

struct Fixture {
    app: Router,
    anonymous: Router,
    store: Arc<dyn DynAuthStore>,
    membership: Option<String>,
    other_tenant: Option<String>,
}
async fn fixture(config: EventsConfig) -> Fixture {
    fixture_backend(
        Arc::new(
            schema_forge_surrealdb::SurrealBackend::connect_memory("events", "events")
                .await
                .unwrap(),
        ),
        config,
    )
    .await
}
async fn fixture_backend<B: SchemaBackend + EntityStore + 'static>(
    backend: Arc<B>,
    config: EventsConfig,
) -> Fixture {
    fixture_options(backend, config, false, false).await
}
async fn fixture_options<B: SchemaBackend + EntityStore + 'static>(
    backend: Arc<B>,
    config: EventsConfig,
    tenancy: bool,
    read_hooks: bool,
) -> Fixture {
    fixture_identity_options(backend, config, tenancy, read_hooks, false).await
}

async fn fixture_identity_options<B: SchemaBackend + EntityStore + 'static>(
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
fn enabled() -> EventsConfig {
    EventsConfig {
        enabled: true,
        keep_alive_secs: 1,
        ..Default::default()
    }
}
async fn request(
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
async fn json(response: axum::response::Response) -> serde_json::Value {
    assert!(
        response.status().is_success(),
        "response status: {}",
        response.status()
    );
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
async fn connect(app: &Router, query: &str) -> Body {
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
async fn frame(body: &mut Body) -> String {
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("SSE deadline")
        .expect("SSE ended")
        .unwrap();
    String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
}
async fn change(body: &mut Body) -> (String, serde_json::Value) {
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
async fn exercise_crud(f: &Fixture) {
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn surreal_crud_events_equal_authorized_get() {
    exercise_crud(&fixture(enabled()).await).await;
}
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_URL; creates Note and User tables"]
async fn postgres_crud_events_equal_authorized_get() {
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").unwrap();
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap(),
    );
    let f = fixture_backend(backend.clone(), enabled()).await;
    backend
        .prepare_record_revisions(&schema_forge_core::types::SchemaName::new("Note").unwrap())
        .await
        .unwrap();
    exercise_crud(&f).await;
    let mut body = connect(&f.app, "").await;
    let fields = serde_json::json!({"title":"Intent create", "category":"books"});
    let receipt = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/create-intents",
            fields.clone(),
        )
        .await,
    )
    .await;
    let id = receipt["id"].as_str().unwrap();
    for attempt in 0..2 {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/schemas/Note/entities")
                    .header("content-type", "application/json")
                    .header("create-intent", id)
                    .body(Body::from(serde_json::json!({"fields":fields}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt == 0 {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            }
        );
        let detail = json(response).await;
        if attempt == 0 {
            let (_, event) = change(&mut body).await;
            assert_eq!(event["entity"], detail);
            assert_eq!(event["actor"], "user:alice");
        }
    }
    assert!(
        frame(&mut body).await.contains("keep-alive"),
        "reconciliation must not publish a duplicate create"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_require_authentication_known_schema_and_readable_equality_filters() {
    let f = fixture(enabled()).await;
    assert_eq!(
        request(
            &f.anonymous,
            "GET",
            "/schemas/Note/events",
            serde_json::Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.app,
            "GET",
            "/schemas/Missing/events",
            serde_json::Value::Null
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    for query in [
        "unknown=x",
        "secret=stored-secret",
        "category__gt=x",
        "limit=1",
        "access_token=secret",
        "category__bad=x",
        "owner.name=x",
    ] {
        assert_eq!(
            request(
                &f.app,
                "GET",
                &format!("/schemas/Note/events?{query}"),
                serde_json::Value::Null
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    assert_eq!(
        request(
            &f.app,
            "GET",
            "/schemas/Note/events?restricted=private",
            serde_json::Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let mut body = connect(&f.app, "?category__eq=books").await;
    let _ = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Other", "category":"music"}),
        )
        .await,
    )
    .await;
    let _ = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Match", "category":"books"}),
        )
        .await,
    )
    .await;
    let (_, event) = change(&mut body).await;
    assert_eq!(event["entity"]["fields"]["title"], "Match");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_streams_are_404() {
    let f = fixture(EventsConfig::default()).await;
    assert_eq!(
        request(
            &f.app,
            "GET",
            "/schemas/Note/events",
            serde_json::Value::Null
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_limits_release_when_body_drops() {
    let f = fixture(EventsConfig {
        max_connections_per_user: 1,
        ..enabled()
    })
    .await;
    let body = connect(&f.app, "").await;
    assert_eq!(
        request(
            &f.app,
            "GET",
            "/schemas/Note/events",
            serde_json::Value::Null
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(body);
    let _body = connect(&f.app, "").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_consumers_disconnect_and_writes_remain_successful() {
    let f = fixture(EventsConfig {
        channel_capacity: 1,
        ..enabled()
    })
    .await;
    let mut body = connect(&f.app, "").await;
    for title in ["One", "Two", "Three"] {
        let _ = json(
            request(
                &f.app,
                "POST",
                "/schemas/Note/entities",
                serde_json::json!({"title":title}),
            )
            .await,
        )
        .await;
    }
    let (_, event) = change(&mut body).await;
    assert_eq!(event["reason"], "slow_consumer");
    assert!(body.frame().await.is_none());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_keep_alive_and_live_account_revocation() {
    let f = fixture(enabled()).await;
    let mut body = connect(&f.app, "").await;
    assert!(frame(&mut body).await.contains("keep-alive"));
    f.store.toggle_user_active("alice").await.unwrap();
    assert_closed(&mut body, "deactivated account").await;
    assert!(body.frame().await.is_none());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_updates_are_delivered_in_commit_order() {
    let f = fixture(enabled()).await;
    let mut body = connect(&f.app, "").await;
    let created = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Original"}),
        )
        .await,
    )
    .await;
    let _ = change(&mut body).await;
    let path = format!("/schemas/Note/entities/{}", created["id"].as_str().unwrap());
    for title in ["One", "Two", "Three"] {
        let _ =
            json(request(&f.app, "PATCH", &path, serde_json::json!({"title":title})).await).await;
    }
    for title in ["One", "Two", "Three"] {
        let (_, event) = change(&mut body).await;
        assert_eq!(event["entity"]["fields"]["title"], title);
    }
    let detail = json(request(&f.app, "GET", &path, serde_json::Value::Null).await).await;
    assert_eq!(detail["fields"]["title"], "Three");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn membership_removal_closes_stream_and_active_tenant_cannot_be_impersonated() {
    let backend = Arc::new(
        schema_forge_surrealdb::SurrealBackend::connect_memory("events", "tenants")
            .await
            .unwrap(),
    );
    let f = fixture_options(backend, enabled(), true, false).await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/schemas/Note/events")
                .header(
                    "x-active-tenant",
                    format!("Org:{}", f.other_tenant.as_ref().unwrap()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut body = connect(&f.app, "").await;
    let _ = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Scoped"}),
        )
        .await,
    )
    .await;
    assert_eq!(
        change(&mut body).await.1["entity"]["fields"]["title"],
        "Scoped"
    );
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/schemas/TenantMembership/entities/{}",
                    f.membership.unwrap()
                ))
                .header("x-test-admin", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let event = assert_closed(&mut body, "removed member").await;
    assert!(event.get("entity").is_none());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_membership_role_allows_stream_and_role_revocation_closes_it() {
    let backend = Arc::new(
        schema_forge_surrealdb::SurrealBackend::connect_memory("events", "scoped_roles")
            .await
            .unwrap(),
    );
    let f = fixture_identity_options(backend, enabled(), true, false, true).await;
    assert!(f
        .store
        .get_user("alice")
        .await
        .unwrap()
        .unwrap()
        .roles
        .is_empty());
    assert_eq!(
        f.store.list_tenant_roles("alice").await.unwrap()[0].role,
        "member"
    );
    let mut body = connect(&f.app, "").await;
    let created = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title": "Scoped role"}),
        )
        .await,
    )
    .await;
    let (_, event) = change(&mut body).await;
    assert_eq!(event["entity"]["id"], created["id"]);
    assert_eq!(event["entity"]["fields"]["title"], "Scoped role");
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/schemas/TenantMembership/entities/{}",
                    f.membership.as_ref().unwrap()
                ))
                .header("x-test-admin", "1")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"fields": {"role": null}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        f.store
            .list_tenant_memberships("alice")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(f.store.list_tenant_roles("alice").await.unwrap().is_empty());
    let event = assert_closed(&mut body, "revoked scoped role").await;
    assert!(event.get("entity").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_denial_never_delivers_entity_or_delete_metadata() {
    let f = fixture(enabled()).await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/schemas/Note/events")
                .header("x-test-subject", "user:bob")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    assert!(frame(&mut body).await.contains("retry:"));
    let created = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Alice only"}),
        )
        .await,
    )
    .await;
    let path = format!("/schemas/Note/entities/{}", created["id"].as_str().unwrap());
    assert_eq!(
        request(&f.app, "DELETE", &path, serde_json::Value::Null)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    // A queued denied create/delete must yield only an idle comment, never metadata.
    assert!(frame(&mut body).await.contains("keep-alive"));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_updates_end_with_the_same_snapshot_as_storage() {
    let f = fixture(enabled()).await;
    let mut body = connect(&f.app, "").await;
    let created = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Initial"}),
        )
        .await,
    )
    .await;
    let _ = change(&mut body).await;
    let path = format!("/schemas/Note/entities/{}", created["id"].as_str().unwrap());
    let writes = (0..6).map(|index| {
        request(
            &f.app,
            "PATCH",
            &path,
            serde_json::json!({"title":format!("Concurrent {index}")}),
        )
    });
    for response in futures::future::join_all(writes).await {
        assert_eq!(response.status(), StatusCode::OK);
    }
    let mut titles = std::collections::HashSet::new();
    let mut last = serde_json::Value::Null;
    for _ in 0..6 {
        last = change(&mut body).await.1["entity"].clone();
        titles.insert(last["fields"]["title"].as_str().unwrap().to_string());
    }
    assert_eq!(titles.len(), 6);
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
    assert_eq!(last, detail);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relation_filters_derived_collections_and_read_hooks_equal_get_projection() {
    let backend = Arc::new(
        schema_forge_surrealdb::SurrealBackend::connect_memory("events", "read_projection")
            .await
            .unwrap(),
    );
    let f = fixture_options(backend, enabled(), false, true).await;
    let parent = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Parent"}),
        )
        .await,
    )
    .await;
    let parent_id = parent["id"].as_str().unwrap();
    let mut filtered = connect(&f.app, &format!("?parent={parent_id}")).await;
    let mut all = connect(&f.app, "").await;
    let child = json(
        request(
            &f.app,
            "POST",
            "/schemas/Note/entities",
            serde_json::json!({"title":"Child", "parent":parent_id}),
        )
        .await,
    )
    .await;
    let (_, event) = change(&mut filtered).await;
    let child_path = format!(
        "/schemas/Note/entities/{}?resolve=false",
        child["id"].as_str().unwrap()
    );
    let detail = json(request(&f.app, "GET", &child_path, serde_json::Value::Null).await).await;
    assert_eq!(event["entity"], detail);
    assert_eq!(detail["fields"]["parent"], parent_id);
    assert_eq!(detail["fields"]["read_label"], "decorated");
    let _ = change(&mut all).await;
    let parent_path = format!("/schemas/Note/entities/{parent_id}");
    let _ = json(
        request(
            &f.app,
            "PATCH",
            &parent_path,
            serde_json::json!({"title":"Updated Parent"}),
        )
        .await,
    )
    .await;
    let (_, event) = change(&mut all).await;
    let detail = json(
        request(
            &f.app,
            "GET",
            &format!("{parent_path}?resolve=false"),
            serde_json::Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(event["entity"], detail);
    assert_eq!(
        detail["fields"]["children"],
        serde_json::json!([child["id"]])
    );
}

// ---------------------------------------------------------------------------
// Production identity path. Tokens are minted by `POST /auth/login`, verified
// by the same PASETO middleware `serve` installs, and rewritten by the tenant
// scope middleware, so the stream sees the real subject (`user:<username>`)
// and the real per-request tenant claims rather than hand-built ones.
// ---------------------------------------------------------------------------

const PASSWORD: &str = "correct-horse-battery-staple";

struct LoginFixture {
    app: Router,
    store: Arc<dyn DynAuthStore>,
    backend: Arc<schema_forge_surrealdb::SurrealBackend>,
    memberships: schema_forge_core::types::SchemaDefinition,
    lobby: String,
    alpha: String,
    beta: String,
    _key: tempfile::NamedTempFile,
}

async fn login_fixture(database: &str) -> LoginFixture {
    use acton_service::{
        auth::{config::TokenGenerationConfig, tokens::paseto_generator::PasetoGenerator},
        config::PasetoConfig,
        middleware::PasetoAuth,
    };
    use schema_forge_acton::{
        authz::{PrincipalClaimMappings, RoleRanks},
        middleware::tenant_scope::{self, TenantScopeState},
        routes::auth::auth_routes,
        SchemaForgeExtension,
    };
    use schema_forge_backend::Entity;
    use schema_forge_core::types::DynamicValue;

    let backend = Arc::new(
        schema_forge_surrealdb::SurrealBackend::connect_memory("events", database)
            .await
            .unwrap(),
    );
    let root = schema_forge_dsl::parse(
        r#"
        @tenant(root)
        @access(read: ["owner", "member"], write: ["owner"], delete: ["owner"])
        schema Organization { name: text required }
    "#,
    )
    .unwrap()
    .remove(0);
    backend
        .apply_migration(&root.name, &DiffEngine::create_new(&root).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&root).await.unwrap();
    let ranks = RoleRanks::from_toml_str("[roles]\nmember = 10\nowner = 20").unwrap();
    let extension = SchemaForgeExtension::builder()
        .with_backend_arc(backend.clone())
        .with_role_ranks(ranks.clone())
        .build()
        .await
        .unwrap();
    let data = extension.state();
    let schemas = data.registry.list().await;
    let system = |name: &str| {
        schemas
            .iter()
            .find(|schema| schema.name.as_str() == name)
            .unwrap()
            .clone()
    };
    let store: Arc<dyn DynAuthStore> = Arc::new(
        EntityAuthStore::new(
            backend.clone(),
            system("User"),
            Arc::new(move |role| ranks.get(role)),
        )
        .with_tenant_membership_schema(system("TenantMembership")),
    );
    let mut tenants = Vec::new();
    for name in ["lobby", "alpha", "beta"] {
        let org = Entity::new(
            root.name.clone(),
            std::collections::BTreeMap::from([("name".into(), DynamicValue::Text(name.into()))]),
        );
        EntityStore::create(backend.as_ref(), &org).await.unwrap();
        tenants.push(org.id.to_string());
    }
    let [lobby, alpha, beta] = <[String; 3]>::try_from(tenants).unwrap();
    // Seeded the way invite acceptance seeds accounts: a password User row plus
    // TenantMembership rows. `founder` holds the owner membership that
    // `[schema_forge.tenancy] creator_role` grants the admin who created lobby.
    let accounts = [
        ("admin@example.gov", "platform_admin", vec![]),
        (
            "founder@example.gov",
            "platform_admin",
            vec![(lobby.as_str(), "owner")],
        ),
        (
            "solo@example.gov",
            "member",
            vec![(lobby.as_str(), "member")],
        ),
        (
            "multi@example.gov",
            "member",
            vec![
                (lobby.as_str(), "member"),
                (alpha.as_str(), "owner"),
                (beta.as_str(), "owner"),
            ],
        ),
    ];
    for (username, role, memberships) in accounts {
        store
            .create_user(username, PASSWORD, &[role.into()], username)
            .await
            .unwrap();
        for (tenant, scoped_role) in memberships {
            store
                .add_tenant_membership(username, "Organization", tenant, Some(scoped_role))
                .await
                .unwrap();
        }
    }
    let service = ServiceBuilder::new()
        .with_config(Config::<SchemaForgeConfig>::default())
        .with_actor::<ForgeActor>()
        .build();
    let handle = service.state().actor::<ForgeActor>().unwrap();
    let (tx, rx) = oneshot::channel();
    handle
        .send(InitForge {
            registry: schemas
                .iter()
                .map(|schema| (schema.name.to_string(), schema.clone()))
                .collect(),
            backend: data.backend.clone(),
            tenant_config: data.tenant_config.clone(),
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: data.storage_registry.clone(),
            policy_store: Some(data.policy_store.clone()),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    let (tx, rx) = oneshot::channel();
    handle
        .send(ConfigureEvents {
            runtime: Arc::new(
                EventsRuntime::new(enabled(), store.clone(), backend.clone()).unwrap(),
            ),
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    let key = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(key.path(), [29; 32]).unwrap();
    let validator = PasetoAuth::new(&PasetoConfig {
        version: "v4".into(),
        purpose: "local".into(),
        key_path: key.path().into(),
        issuer: None,
        audience: None,
        public_paths: vec!["/forge/auth/login".into()],
    })
    .unwrap();
    let generator = Arc::new(PasetoGenerator::with_symmetric_key(
        [29; 32],
        TokenGenerationConfig::default(),
    ));
    let app = Router::new()
        .nest(
            "/forge",
            forge_routes()
                .merge(auth_routes())
                .layer(axum::Extension(store.clone()))
                .layer(axum::Extension(generator))
                .layer(axum::Extension(Arc::new(PrincipalClaimMappings::default())))
                .layer(axum::Extension(Arc::new(data.tenant_config.clone()))),
        )
        .layer(axum::middleware::from_fn_with_state(
            TenantScopeState {
                entity_store: backend.clone(),
                tenant_config: Arc::new(data.tenant_config.clone()),
            },
            tenant_scope::middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            validator,
            PasetoAuth::middleware,
        ))
        .with_state(service.state().clone());
    LoginFixture {
        app,
        store,
        backend,
        memberships: system("TenantMembership"),
        lobby,
        alpha,
        beta,
        _key: key,
    }
}

impl LoginFixture {
    async fn login(&self, username: &str) -> String {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/forge/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username": username, "password": PASSWORD}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        json(response).await["token"].as_str().unwrap().to_owned()
    }

    async fn subscribe(&self, token: &str, tenant: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder()
            .uri("/forge/schemas/Organization/events")
            .header("accept", "text/event-stream")
            .header("authorization", format!("Bearer {token}"));
        if let Some(tenant) = tenant {
            request = request.header("x-active-tenant", format!("Organization:{tenant}"));
        }
        self.app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// Opens a stream and returns its body after the initial retry frame.
    async fn open(&self, token: &str, tenant: Option<&str>, caller: &str) -> Body {
        let response = self.subscribe(token, tenant).await;
        let status = response.status();
        let mut body = response.into_body();
        if status != StatusCode::OK {
            let bytes = body.collect().await.unwrap().to_bytes();
            panic!(
                "{caller} stream (tenant {tenant:?}) refused with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        assert!(frame(&mut body).await.contains("retry:"));
        body
    }

    async fn assert_opens(&self, token: &str, tenant: Option<&str>, caller: &str) {
        drop(self.open(token, tenant, caller).await);
    }

    async fn refused(&self, token: &str, tenant: Option<&str>, caller: &str) {
        let status = self.subscribe(token, tenant).await.status();
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{caller} stream (tenant {tenant:?}) must be refused"
        );
    }

    /// Changes or removes one durable membership row behind the caller's token.
    async fn set_membership(&self, username: &str, tenant: &str, role: Option<&str>) {
        use schema_forge_core::types::DynamicValue;
        let user = self.store.get_user_entity(username).await.unwrap().unwrap();
        let rows = self
            .backend
            .query(&schema_forge_core::query::Query::new(
                self.memberships.id.clone(),
            ))
            .await
            .unwrap()
            .entities;
        let mut row = rows
            .into_iter()
            .find(|row| {
                row.field("user") == Some(&DynamicValue::Ref(user.id.clone()))
                    && row.field("tenant_id") == Some(&DynamicValue::Text(tenant.into()))
            })
            .unwrap();
        match role {
            Some(role) => {
                row.fields
                    .insert("role".into(), DynamicValue::Text(role.into()));
                EntityStore::update(self.backend.as_ref(), &row)
                    .await
                    .unwrap();
            }
            None => EntityStore::delete(self.backend.as_ref(), &row.schema, &row.id)
                .await
                .unwrap(),
        }
    }
}

async fn assert_closed(body: &mut Body, caller: &str) -> serde_json::Value {
    // Keep-alives arrive every second, so an unrevoked stream would never time
    // out inside `frame`; bound the wait for the authorization check instead.
    let (frame, event) = tokio::time::timeout(Duration::from_secs(10), change(body))
        .await
        .unwrap_or_else(|_| panic!("{caller} stream stayed open after revocation"));
    assert!(
        frame.contains("event: closed"),
        "{caller} stream must close, got {frame}"
    );
    assert_eq!(event["reason"], "authorization_changed");
    event
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_tokens_open_streams_for_admins_and_tenant_members() {
    let f = login_fixture("login_open").await;
    let admin = f.login("admin@example.gov").await;
    f.assert_opens(&admin, None, "platform admin").await;
    f.assert_opens(&admin, Some(&f.lobby), "platform admin")
        .await;

    let founder = f.login("founder@example.gov").await;
    f.assert_opens(&founder, None, "platform admin with a creator membership")
        .await;
    f.assert_opens(
        &founder,
        Some(&f.lobby),
        "platform admin with a creator membership",
    )
    .await;

    let solo = f.login("solo@example.gov").await;
    f.assert_opens(&solo, None, "single-membership member")
        .await;
    f.assert_opens(&solo, Some(&f.lobby), "single-membership member")
        .await;

    let multi = f.login("multi@example.gov").await;
    for tenant in [&f.lobby, &f.alpha, &f.beta] {
        f.assert_opens(&multi, Some(tenant), "multi-membership user")
            .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_streams_close_and_stay_refused_after_membership_changes() {
    let f = login_fixture("login_memberships").await;
    let solo = f.login("solo@example.gov").await;
    let mut stream = f.open(&solo, None, "single-membership member").await;
    f.set_membership("solo@example.gov", &f.lobby, None).await;
    assert_closed(&mut stream, "removed member").await;
    f.refused(&solo, None, "removed member").await;
    f.refused(&solo, Some(&f.lobby), "removed member").await;

    let multi = f.login("multi@example.gov").await;
    let mut stream = f
        .open(&multi, Some(&f.alpha), "multi-membership owner")
        .await;
    f.set_membership("multi@example.gov", &f.alpha, Some("member"))
        .await;
    assert_closed(&mut stream, "demoted owner").await;
    f.refused(&multi, Some(&f.alpha), "demoted owner").await;
    // The unchanged scoped grant behind the same token still authorizes.
    f.assert_opens(&multi, Some(&f.beta), "multi-membership owner")
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_streams_close_and_stay_refused_after_account_changes() {
    let f = login_fixture("login_accounts").await;
    let admin = f.login("admin@example.gov").await;
    let mut stream = f.open(&admin, None, "platform admin").await;
    f.store
        .toggle_user_active("admin@example.gov")
        .await
        .unwrap();
    assert_closed(&mut stream, "deactivated admin").await;
    f.refused(&admin, None, "deactivated admin").await;

    let founder = f.login("founder@example.gov").await;
    let mut stream = f
        .open(&founder, None, "platform admin with a creator membership")
        .await;
    f.store
        .update_user("founder@example.gov", &["member".into()], "Founder")
        .await
        .unwrap();
    assert_closed(&mut stream, "demoted admin").await;
    f.refused(&founder, None, "demoted admin").await;
    f.refused(&founder, Some(&f.lobby), "demoted admin").await;
}
