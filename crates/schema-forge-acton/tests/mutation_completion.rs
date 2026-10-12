//! Deliberately gated storage verifies HTTP acknowledgement and actor-owned effects.
//! This fixture models delayed completion, not database transaction semantics.
mod support;
use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Extension, Router,
};
use schema_forge_acton::{
    config::SchemaForgeConfig,
    hooks::{HookBinding, HookDispatcher, HookError, HookInvocation, HookOutcome, HooksConfig},
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    ForgeActor, HookDispatchActor,
};
#[cfg(feature = "sse")]
use schema_forge_acton::{
    events::{ConfigureEvents, EventsRuntime},
    events_config::EventsConfig,
};
use schema_forge_backend::{BackendError, Entity, EntityStore, QueryResult, SchemaBackend};
use schema_forge_core::{
    migration::MigrationStep,
    query::{AggregateQuery, AggregateResult, Query},
    types::{DynamicValue, EntityId, HookEvent, SchemaDefinition, SchemaName},
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};
use tower::ServiceExt;

struct GatedBackend {
    reads: support::FixtureBackend,
    row: Mutex<Entity>,
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
    subscriptions: SchemaDefinition,
    queried: mpsc::UnboundedSender<()>,
}
impl GatedBackend {
    async fn gate(&self) {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
    }
}
impl SchemaBackend for GatedBackend {
    async fn apply_migration(
        &self,
        name: &SchemaName,
        steps: &[MigrationStep],
    ) -> Result<(), BackendError> {
        self.reads.apply_migration(name, steps).await
    }
    async fn store_schema_metadata(&self, schema: &SchemaDefinition) -> Result<(), BackendError> {
        self.reads.store_schema_metadata(schema).await
    }
    async fn load_schema_metadata(
        &self,
        name: &SchemaName,
    ) -> Result<Option<SchemaDefinition>, BackendError> {
        if name == &self.subscriptions.name {
            return Ok(Some(self.subscriptions.clone()));
        }
        self.reads.load_schema_metadata(name).await
    }
    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        self.reads.list_schema_metadata().await
    }
}
impl EntityStore for GatedBackend {
    async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
        self.gate().await;
        if entity.fields.get("title") == Some(&DynamicValue::Text("fail".into())) {
            return Err(BackendError::Internal {
                message: "deliberate failed write".into(),
            });
        }
        *self.row.lock().await = entity.clone();
        Ok(entity.clone())
    }
    async fn get(&self, _: &SchemaName, _: &EntityId) -> Result<Entity, BackendError> {
        Ok(self.row.lock().await.clone())
    }
    async fn update(&self, entity: &Entity) -> Result<Entity, BackendError> {
        self.gate().await;
        let mut row = self.row.lock().await;
        row.fields.extend(entity.fields.clone());
        Ok(row.clone())
    }
    async fn delete(&self, _: &SchemaName, _: &EntityId) -> Result<(), BackendError> {
        self.gate().await;
        Ok(())
    }
    async fn query(&self, query: &Query) -> Result<QueryResult, BackendError> {
        if query.schema == self.subscriptions.id {
            self.queried.send(()).unwrap();
            return Ok(QueryResult::new(vec![], None));
        }
        self.reads.query(query).await
    }
    async fn count(&self, query: &Query) -> Result<usize, BackendError> {
        self.reads.count(query).await
    }
    async fn aggregate(
        &self,
        query: &AggregateQuery,
    ) -> Result<Vec<AggregateResult>, BackendError> {
        self.reads.aggregate(query).await
    }
}
#[derive(Debug)]
struct CaptureHooks(mpsc::UnboundedSender<HookInvocation>);
#[async_trait::async_trait]
impl HookDispatcher for CaptureHooks {
    async fn call_before(
        &self,
        _: &HookBinding,
        _: HookInvocation,
    ) -> Result<HookOutcome, HookError> {
        Ok(HookOutcome::default())
    }
    async fn call_after(
        &self,
        _: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<(), HookError> {
        self.0.send(invocation).unwrap();
        Ok(())
    }
}
struct Fixture {
    router: Router,
    backend: Arc<GatedBackend>,
    entered: mpsc::UnboundedReceiver<()>,
    hooks: mpsc::UnboundedReceiver<HookInvocation>,
    path: String,
    queries: mpsc::UnboundedReceiver<()>,
    audit: tokio::net::UdpSocket,
}
async fn fixture() -> Fixture {
    let schema = schema_forge_dsl::parse(
        r#"
        @access(read: ["writer"], write: ["writer"], delete: ["writer"])
        @webhook(events: ["created", "updated", "deleted"])
        @hook(after_change) """capture changes"""
        @hook(after_delete) """capture deletes"""
        schema Note { title: text required }
    "#,
    )
    .unwrap()
    .remove(0);
    let row = Entity::new(
        schema.name.clone(),
        BTreeMap::from([("title".into(), DynamicValue::Text("before".into()))]),
    );
    let path = format!("/schemas/Note/entities/{}", row.id);
    let (started, entered) = mpsc::unbounded_channel();
    let (queried, queries) = mpsc::unbounded_channel();
    let backend = Arc::new(GatedBackend {
        reads: support::FixtureBackend::new(schema.clone(), vec![row.clone()]),
        row: Mutex::new(row),
        entered: started,
        release: Semaphore::new(0),
        subscriptions: schema_forge_dsl::parse(
            "schema WebhookSubscription { target_schema: text active: boolean }",
        )
        .unwrap()
        .remove(0),
        queried,
    });
    let (hook_tx, hooks) = mpsc::unbounded_channel();
    let mut custom = SchemaForgeConfig::default();
    custom.schema_forge.webhooks.enabled = true;
    custom.schema_forge.hooks = HooksConfig {
        enabled: true,
        bindings: [HookEvent::AfterChange, HookEvent::AfterDelete]
            .into_iter()
            .map(|event| HookBinding {
                schema: "Note".into(),
                event,
                endpoint: "http://unused".into(),
                timeout_ms: None,
                required: false,
                descriptor_path: None,
            })
            .collect(),
        ..Default::default()
    };
    let audit = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let service = ServiceBuilder::new()
        .with_config(Config {
            custom,
            audit: Some(acton_service::audit::config::AuditConfig {
                syslog: acton_service::audit::config::SyslogConfig {
                    transport: "udp".into(),
                    address: audit.local_addr().unwrap().to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Config::default()
        })
        .with_actor::<ForgeActor>()
        .with_actor::<HookDispatchActor>()
        .build();
    let actor = service.state().actor::<ForgeActor>().unwrap();
    let (tx, rx) = oneshot::channel();
    actor
        .send(InitForge {
            registry: HashMap::from([("Note".into(), schema)]),
            backend: backend.clone(),
            tenant_config: None,
            record_access_policy: None,
            hook_dispatcher: Some(Arc::new(CaptureHooks(hook_tx))),
            storage_registry: Default::default(),
            policy_store: None,
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    // Exercise the serialized commit handlers as well as the ordinary actor
    // handlers when the runtime's SSE feature is enabled.
    #[cfg(feature = "sse")]
    {
        let user = schema_forge_dsl::parse(schema_forge_core::system_schemas::USER_SCHEMA)
            .unwrap()
            .remove(0);
        let store = Arc::new(schema_forge_backend::EntityAuthStore::new(
            backend.clone(),
            user,
            Arc::new(|_| Some(10)),
        ));
        let (tx, rx) = oneshot::channel();
        actor
            .send(ConfigureEvents {
                runtime: Arc::new(
                    EventsRuntime::new(
                        EventsConfig {
                            enabled: true,
                            ..Default::default()
                        },
                        store,
                        backend.clone(),
                    )
                    .unwrap(),
                ),
                reply: ReplyChannel::new(tx),
            })
            .await;
        rx.await.unwrap();
    }
    let caller = Claims {
        sub: "user:writer".into(),
        roles: vec!["writer".into()],
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
    let router = forge_routes()
        .layer(Extension(caller))
        .with_state(service.state().clone());
    Fixture {
        router,
        backend,
        entered,
        hooks,
        path,
        queries,
        audit,
    }
}
fn request(method: &str, path: &str, title: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"fields":{"title":title}}).to_string(),
        ))
        .unwrap()
}
async fn next<T>(receiver: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn committed_audit(socket: &tokio::net::UdpSocket, method: &str) {
    let expected = match method {
        "POST" => "forge.entity.created",
        "PUT" => "forge.entity.updated",
        "PATCH" => "forge.entity.patched",
        _ => "forge.entity.deleted",
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut buffer = [0; 8192];
        loop {
            let length = socket.recv(&mut buffer).await.unwrap();
            let event = std::str::from_utf8(&buffer[..length]).unwrap();
            if event.contains(expected) {
                assert!(event.contains("user:writer"));
                assert!(event.contains("Note"));
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_create_and_updates_acknowledge_commits_after_five_seconds() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let mut fixture = fixture().await;
        let path = if method == "POST" {
            "/schemas/Note/entities"
        } else {
            &fixture.path
        };
        let work = tokio::spawn(
            fixture
                .router
                .clone()
                .oneshot(request(method, path, "after")),
        );
        next(&mut fixture.entered).await;
        // Deliberately exceed the former reply deadline after storage starts.
        tokio::time::sleep(Duration::from_millis(5200)).await;
        fixture.backend.release.add_permits(1);
        let response = work.await.unwrap().unwrap();
        assert_eq!(
            response.status(),
            match method {
                "POST" => StatusCode::CREATED,
                "DELETE" => StatusCode::NO_CONTENT,
                _ => StatusCode::OK,
            }
        );
        let hook = next(&mut fixture.hooks).await;
        next(&mut fixture.queries).await;
        committed_audit(&fixture.audit, method).await;
        assert_eq!(
            hook.operation,
            match method {
                "POST" => "create",
                "PUT" => "update",
                "PATCH" => "patch",
                _ => "delete",
            }
        );
        if method != "DELETE" {
            assert_eq!(
                fixture.backend.row.lock().await.fields["title"],
                DynamicValue::Text("after".into())
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_effects_survive_dropped_http_requests_for_every_mutation() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let mut fixture = fixture().await;
        let path = if method == "POST" {
            "/schemas/Note/entities"
        } else {
            &fixture.path
        };
        let work = tokio::spawn(
            fixture
                .router
                .clone()
                .oneshot(request(method, path, "after")),
        );
        next(&mut fixture.entered).await;
        work.abort();
        assert!(work.await.unwrap_err().is_cancelled());
        fixture.backend.release.add_permits(1);
        let hook = next(&mut fixture.hooks).await;
        next(&mut fixture.queries).await;
        committed_audit(&fixture.audit, method).await;
        assert_eq!(
            hook.fields["title"],
            DynamicValue::Text(
                if method == "DELETE" {
                    "before"
                } else {
                    "after"
                }
                .into()
            )
        );
        if method == "PUT" || method == "PATCH" {
            assert_eq!(hook.changed_fields, ["title"]);
            assert_eq!(hook.previous["title"], DynamicValue::Text("before".into()));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_storage_does_not_dispatch_success_effects() {
    let mut fixture = fixture().await;
    let work = tokio::spawn(fixture.router.oneshot(request(
        "POST",
        "/schemas/Note/entities",
        "fail",
    )));
    next(&mut fixture.entered).await;
    fixture.backend.release.add_permits(1);
    assert_eq!(
        work.await.unwrap().unwrap().status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), fixture.hooks.recv())
            .await
            .is_err()
    );
    assert_eq!(
        fixture.backend.row.lock().await.fields["title"],
        DynamicValue::Text("before".into())
    );
}
