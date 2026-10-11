//! Real storage regressions for tenant-bound field query proofs.
#![cfg(feature = "test-surrealdb")]

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
    authz::{PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks},
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    storage::StorageRegistry,
    ForgeActor,
};
use schema_forge_backend::{Entity, EntityStore, SchemaBackend, TenantConfig};
use schema_forge_core::{migration::DiffEngine, types::DynamicValue};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::oneshot;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    state: acton_service::state::AppState<SchemaForgeConfig>,
    schemas: Vec<schema_forge_core::types::SchemaDefinition>,
    #[cfg(feature = "graphql")]
    backend: Arc<schema_forge_surrealdb::SurrealBackend>,
    store: Arc<PolicyStore>,
    tenant_config: Option<TenantConfig>,
    allowed: String,
    shared: String,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_options(true, "").await
    }

    async fn with_options(enforce_tenancy: bool, extra_policy: &str) -> Self {
        let schemas = schema_forge_dsl::parse(
            r#"
            @tenant(root)
            @access(read: ["member"], write: ["member"], delete: ["member"])
            schema Organization { name: text required @field_access(write: ["member"]) }
            @tenant(parent: "Organization")
            @export(formats: [csv], bundle_files: false, max_rows: 100)
            @access(read: ["member"], write: ["member"], delete: ["member"])
            schema Contact {
                organization: -> Organization required
                name: text required @field_access(write: ["member"]) @exportable
            }
        "#,
        )
        .unwrap();
        let backend = Arc::new(
            schema_forge_surrealdb::test_support::connect("tenant_proof", "tenant_proof")
                .await
                .unwrap(),
        );
        for schema in &schemas {
            backend
                .apply_migration(&schema.name, &DiffEngine::create_new(schema).steps)
                .await
                .unwrap();
            backend.store_schema_metadata(schema).await.unwrap();
        }
        let mut tenants = Vec::new();
        for name in ["Allowed", "Shared"] {
            let root = Entity::new(
                schemas[0].name.clone(),
                BTreeMap::from([("name".into(), DynamicValue::Text(name.into()))]),
            );
            backend.create(&root).await.unwrap();
            let contact = Entity::new(
                schemas[1].name.clone(),
                BTreeMap::from([
                    ("organization".into(), DynamicValue::Ref(root.id.clone())),
                    ("name".into(), DynamicValue::Text(format!("Ada {name}"))),
                    ("_tenant".into(), DynamicValue::Text(root.id.to_string())),
                ]),
            );
            backend.create(&contact).await.unwrap();
            tenants.push(root.id.to_string());
        }
        let policies = tempfile::tempdir().unwrap();
        std::fs::write(
            policies.path().join("custom.cedar"),
            format!(
                r#"
            @id("shared_tenant_inert")
            forbid(principal, action, resource) when {{
                resource has "_tenant"
                && resource["_tenant"] == Forge::Tenant::"{}"
                && !(principal in Forge::Group::"platform_admin")
            }};
            permit(principal, action == Action::"ExportContact", resource is Contact);
        "#,
                tenants[1]
            ),
        )
        .unwrap();
        let mut custom = std::fs::read_to_string(policies.path().join("custom.cedar")).unwrap();
        custom.push_str(extra_policy);
        std::fs::write(policies.path().join("custom.cedar"), custom).unwrap();
        let store = Arc::new(PolicyStore::new(
            PolicyStoreSnapshot::from_schemas(
                &schemas,
                Some(policies.path()),
                RoleRanks::empty(),
                PrincipalClaimMappings::default(),
            )
            .unwrap(),
        ));
        let service = ServiceBuilder::new()
            .with_config(Config::<SchemaForgeConfig>::default())
            .with_actor::<ForgeActor>()
            .with_actor::<schema_forge_acton::ExportRateLimiter>()
            .with_actor::<schema_forge_acton::ExportJobActor>()
            .build();
        let tenant_config = enforce_tenancy.then(|| TenantConfig::from_schemas(&schemas).unwrap());
        let (tx, rx) = oneshot::channel();
        service
            .state()
            .actor::<ForgeActor>()
            .unwrap()
            .send(InitForge {
                registry: schemas
                    .iter()
                    .map(|schema| (schema.name.to_string(), schema.clone()))
                    .collect(),
                backend: backend.clone(),
                tenant_config: tenant_config.clone(),
                record_access_policy: None,
                hook_dispatcher: None,
                storage_registry: StorageRegistry::default(),
                policy_store: Some(store.clone()),
                custom_policies_dir: None,
                reply: ReplyChannel::new(tx),
            })
            .await;
        rx.await.unwrap();
        Self {
            app: forge_routes().with_state(service.state().clone()),
            state: service.state().clone(),
            schemas,
            #[cfg(feature = "graphql")]
            backend,
            store,
            tenant_config,
            allowed: tenants.remove(0),
            shared: tenants.remove(0),
        }
    }

    fn claims(&self, admin: bool, tenants: &[&str]) -> Claims {
        Claims {
            sub: "user:tenant-proof".into(),
            roles: vec![if admin { "platform_admin" } else { "member" }.into()],
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
                Value::Array(
                    tenants
                        .iter()
                        .map(|tenant| json!({"schema":"Organization","entity_id":tenant}))
                        .collect(),
                ),
            )]),
        }
    }

    async fn get(&self, path: &str, claims: Claims) -> (StatusCode, Value) {
        let mut request = Request::get(path).body(Body::empty()).unwrap();
        request.extensions_mut().insert(claims);
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_tenant_can_filter_and_sort_the_reported_field() {
    let fixture = Fixture::new().await;
    for query in ["name__startswith=Ada", "sort=name"] {
        let (status, body) = fixture
            .get(
                &format!("/schemas/Contact/entities?{query}&resolve=false"),
                fixture.claims(false, &[&fixture.allowed]),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entities"].as_array().unwrap().len(), 1, "{body}");
        assert_eq!(
            body["entities"][0]["fields"]["name"], "Ada Allowed",
            "{body}"
        );
        assert_eq!(body["total_count"], 1, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_can_filter_and_sort_across_all_tenants() {
    let fixture = Fixture::new().await;
    for query in ["name__startswith=Ada", "sort=name"] {
        let (status, body) = fixture
            .get(
                &format!("/schemas/Contact/entities?{query}&resolve=false"),
                fixture.claims(true, &[]),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entities"].as_array().unwrap().len(), 2, "{body}");
        assert_eq!(body["total_count"], 2, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forbidden_candidate_including_an_ancestor_denies_the_entire_predicate() {
    let fixture = Fixture::new().await;
    for tenants in [
        vec![fixture.shared.as_str()],
        vec![fixture.shared.as_str(), fixture.allowed.as_str()],
    ] {
        for query in ["name__startswith=Ada", "sort=name"] {
            let (status, body) = fixture
                .get(
                    &format!("/schemas/Contact/entities?{query}&resolve=false"),
                    fixture.claims(false, &tenants),
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roots_bind_the_tenant_to_the_enforced_record_id() {
    let fixture = Fixture::new().await;
    let (status, body) = fixture
        .get(
            "/schemas/Organization/entities?sort=name&resolve=false",
            fixture.claims(false, &[&fixture.allowed]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["entities"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body["entities"][0]["id"], fixture.allowed, "{body}");
    let (status, body) = fixture
        .get(
            "/schemas/Organization/entities?sort=name",
            fixture.claims(false, &[&fixture.shared, &fixture.allowed]),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_enforcement_or_empty_scope_cannot_certify_a_tenant_from_claims() {
    for enabled in [false, true] {
        let fixture = Fixture::with_options(enabled, "").await;
        let tenants = if enabled {
            vec![]
        } else {
            vec![fixture.allowed.as_str()]
        };
        let (status, body) = fixture
            .get(
                "/schemas/Contact/entities?sort=name",
                fixture.claims(false, &tenants),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_record_fences_still_require_concrete_field_authorization() {
    let fixture = Fixture::with_options(
        true,
        r#"
        forbid(principal, action == Action::"ReadFieldContact_name", resource is Contact)
        when { resource.name == "Ada Allowed" };
    "#,
    )
    .await;
    for admin in [false, true] {
        let (status, body) = fixture
            .get(
                "/schemas/Contact/entities?sort=name",
                fixture.claims(admin, &[&fixture.allowed]),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_queries_share_the_same_scope_for_proof_and_storage() {
    let fixture = Fixture::new().await;
    for (tenants, expected) in [
        (vec![fixture.allowed.as_str()], StatusCode::OK),
        (
            vec![fixture.shared.as_str(), fixture.allowed.as_str()],
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = Request::post("/schemas/Contact/entities/query")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "filter":{"op":"startswith","field":"name","value":"Ada"},
                    "sort":[{"field":"name","order":"asc"}],"resolve":false
                })
                .to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(fixture.claims(false, &tenants));
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["entities"].as_array().unwrap().len(), 1, "{body}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_acceptance_and_materialization_recheck_the_current_policy_and_scope() {
    use schema_forge_acton::routes::export::{prepare_export, ExportContext};
    let fixture = Fixture::new().await;
    let filter = json!({"op":"startswith","field":"name","value":"Ada"});
    for (tenants, expected) in [
        (vec![fixture.allowed.as_str()], StatusCode::OK),
        (
            vec![fixture.shared.as_str(), fixture.allowed.as_str()],
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = Request::post("/schemas/Contact/entities/export")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"format":"csv","filter":filter,"fields":["name"]}).to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(fixture.claims(false, &tenants));
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let csv = std::str::from_utf8(&bytes).unwrap();
            assert!(csv.contains("Ada Allowed"), "{csv}");
            assert!(!csv.contains("Ada Shared"), "{csv}");
        }
    }
    // Async execution uses this same public materialization boundary. A policy
    // change after acceptance must invalidate the previously accepted filter.
    let claims = fixture.claims(false, &[&fixture.allowed]);
    let forge = fixture.state.actor::<ForgeActor>().unwrap();
    let context = ExportContext {
        forge: &forge,
        claims: Some(&claims),
        tenant_config: &fixture.tenant_config,
        policy_store: &fixture.store,
        record_access_policy: &None,
    };
    assert!(
        prepare_export(&context, &fixture.schemas[1], Some(&filter), None, 100)
            .await
            .is_ok()
    );
    let policies = tempfile::tempdir().unwrap();
    std::fs::write(
        policies.path().join("deny.cedar"),
        r#"forbid(principal, action == Action::"ReadFieldContact_name", resource is Contact);"#,
    )
    .unwrap();
    fixture.store.swap(
        PolicyStoreSnapshot::from_schemas(
            &fixture.schemas,
            Some(policies.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap(),
    );
    assert!(matches!(
        prepare_export(&context, &fixture.schemas[1], Some(&filter), None, 100).await,
        Err(schema_forge_acton::error::ForgeError::Forbidden { .. })
    ));
}

#[derive(Debug, Default)]
struct MemoryArtifacts(std::sync::Mutex<BTreeMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl schema_forge_acton::storage::ExportArtifactStore for MemoryArtifacts {
    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _: &str,
    ) -> Result<(), schema_forge_acton::storage::StorageError> {
        self.0.lock().unwrap().insert(key.into(), bytes);
        Ok(())
    }

    async fn presign_get(
        &self,
        key: &str,
        _: Option<u64>,
    ) -> Result<String, schema_forge_acton::storage::StorageError> {
        Ok(format!("https://example.test/{key}"))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, schema_forge_acton::storage::StorageError> {
        self.0.lock().unwrap().get(key).cloned().ok_or_else(|| {
            schema_forge_acton::storage::StorageError::Request("missing artifact".into())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_export_acceptance_and_execution_retain_the_complete_tenant_scope() {
    use schema_forge_acton::export_job::{
        ExportJobSpec, ExportJobStatus, GetExportJob, StartExportJob,
    };
    let fixture = Fixture::new().await;
    let artifacts = Arc::new(MemoryArtifacts::default());
    let jobs = fixture
        .state
        .actor::<schema_forge_acton::ExportJobActor>()
        .unwrap();
    for (tenants, success) in [
        (vec![fixture.allowed.as_str()], true),
        (
            vec![fixture.shared.as_str(), fixture.allowed.as_str()],
            false,
        ),
    ] {
        let claims = fixture.claims(false, &tenants);
        let filter = json!({"op":"startswith","field":"name","value":"Ada"});
        let mut request = Request::post("/schemas/Contact/entities/export")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"format":"csv","filter":filter,"async":true}).to_string(),
            ))
            .unwrap();
        request.extensions_mut().insert(claims.clone());
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        // No S3 registry is configured. Passing preflight reaches that distinct
        // storage gate; a forbidden tenant is refused before job acceptance.
        assert_eq!(
            response.status(),
            if success {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::FORBIDDEN
            }
        );

        let job_id = schema_forge_acton::export_job::new_job_id();
        let object_key = format!("exports/{job_id}.csv");
        jobs.send(StartExportJob::new(ExportJobSpec {
            job_id: job_id.clone(),
            schema_name: "Contact".into(),
            schema_def: fixture.schemas[1].clone(),
            format: schema_forge_core::types::ExportFormat::Csv,
            filter: Some(filter),
            fields: Some(vec!["name".into()]),
            max_rows: 100,
            bundle_files: false,
            claims: Some(claims),
            tenant_config: fixture.tenant_config.clone(),
            policy_store: fixture.store.clone(),
            record_access_policy: None,
            forge: fixture.state.actor::<ForgeActor>().unwrap().clone(),
            store: artifacts.clone(),
            object_key: object_key.clone(),
            audit_logger: None,
            subject: Some("user:tenant-proof".into()),
        }))
        .await;
        let record = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let (tx, rx) = oneshot::channel();
                jobs.send(GetExportJob {
                    job_id: job_id.clone(),
                    reply: ReplyChannel::new(tx),
                })
                .await;
                if let Some(record) = rx.await.unwrap() {
                    if matches!(
                        record.status,
                        ExportJobStatus::Complete | ExportJobStatus::Failed
                    ) {
                        break record;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            record.status,
            if success {
                ExportJobStatus::Complete
            } else {
                ExportJobStatus::Failed
            },
            "{record:?}"
        );
        let objects = artifacts.0.lock().unwrap();
        if success {
            assert_eq!(record.row_count, Some(1));
            let csv = std::str::from_utf8(objects.get(&object_key).unwrap()).unwrap();
            assert!(csv.contains("Ada Allowed"), "{csv}");
            assert!(!csv.contains("Ada Shared"), "{csv}");
        } else {
            assert!(!objects.contains_key(&object_key));
        }
    }
}

#[cfg(feature = "graphql")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graphql_uses_the_same_complete_tenant_domain() {
    use schema_forge_acton::{
        graphql::{context::ForgeGraphqlContext, schema_builder::build_graphql_schema},
        state::{ForgeState, SchemaRegistry},
    };
    let fixture = Fixture::new().await;
    let registry = SchemaRegistry::new();
    for schema in &fixture.schemas {
        registry
            .insert(schema.name.to_string(), schema.clone())
            .await;
    }
    let schema = build_graphql_schema(&fixture.schemas).unwrap();
    for (admin, tenants, allowed, count) in [
        (false, vec![fixture.allowed.as_str()], true, 1),
        (true, vec![], true, 2),
        (
            false,
            vec![fixture.shared.as_str(), fixture.allowed.as_str()],
            false,
            0,
        ),
    ] {
        let context = ForgeGraphqlContext {
            state: ForgeState {
                registry: registry.clone(),
                backend: fixture.backend.clone(),
                tenant_config: fixture.tenant_config.clone(),
                record_access_policy: None,
                policy_store: fixture.store.clone(),
                graphql_schema: schema_forge_acton::graphql::empty_graphql_schema(),
                auth_store: None,
                webhook_dispatcher: None,
                storage_registry: StorageRegistry::default(),
            },
            app_state: fixture.state.clone(),
            claims: Some(fixture.claims(admin, &tenants)),
        };
        let response = schema.execute(async_graphql::Request::new(r#"{ contacts(filter: {name_starts_with: "Ada"}, sort: [{field: name, order: ASC}]) { items { name } } }"#).data(context)).await;
        assert_eq!(response.errors.is_empty(), allowed, "{:?}", response.errors);
        if allowed {
            assert_eq!(
                response.data.into_json().unwrap()["contacts"]["items"]
                    .as_array()
                    .unwrap()
                    .len(),
                count
            );
        }
    }
}
