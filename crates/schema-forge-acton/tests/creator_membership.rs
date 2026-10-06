//! Root creation and refreshed tenant access for issue 208.
use acton_service::{
    auth::{config::TokenGenerationConfig, tokens::paseto_generator::PasetoGenerator},
    config::{Config, PasetoConfig},
    middleware::{Claims, PasetoAuth, TokenValidator},
    prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Extension, Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    authz::{PrincipalClaimMappings, RoleRanks},
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    middleware::tenant_scope::{self, TenantScopeState},
    routes::{auth::auth_routes, forge_routes},
    state::DynAuthStore,
    ForgeActor, SchemaForgeExtension,
};
use schema_forge_backend::{Entity, EntityAuthStore, EntityStore, SchemaBackend};
use schema_forge_core::{migration::DiffEngine, types::DynamicValue};
use schema_forge_surrealdb::SurrealBackend;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::oneshot;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    store: Arc<dyn DynAuthStore>,
    claims: Claims,
    validator: PasetoAuth,
    _key: tempfile::NamedTempFile,
}
async fn fixture(creator_role: Option<&str>) -> Fixture {
    let backend = Arc::new(
        SurrealBackend::connect_memory("creator", "creator")
            .await
            .unwrap(),
    );
    let root = schema_forge_dsl::parse(
        r#"
        @tenant(root)
        @access(read: ["owner"], write: ["member"])
        schema Organization { name: text required slug: text required unique }
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
    let user = schemas
        .iter()
        .find(|s| s.name.as_str() == "User")
        .unwrap()
        .clone();
    let membership = schemas
        .iter()
        .find(|s| s.name.as_str() == "TenantMembership")
        .unwrap()
        .clone();
    let store: Arc<dyn DynAuthStore> = Arc::new(
        EntityAuthStore::new(backend.clone(), user, Arc::new(move |role| ranks.get(role)))
            .with_tenant_membership_schema(membership),
    );
    store
        .create_user(
            "creator@example.gov",
            "strong-password",
            &["member".into()],
            "Creator",
        )
        .await
        .unwrap();
    let first = Entity::new(
        root.name.clone(),
        BTreeMap::from([
            ("name".into(), DynamicValue::Text("Existing".into())),
            ("slug".into(), DynamicValue::Text("existing".into())),
        ]),
    );
    EntityStore::create(backend.as_ref(), &first).await.unwrap();
    store
        .add_tenant_membership(
            "creator@example.gov",
            "Organization",
            first.id.as_str(),
            Some("member"),
        )
        .await
        .unwrap();
    let mut config = Config::<SchemaForgeConfig>::default();
    config.custom.schema_forge.tenancy.creator_role = creator_role.map(str::to_owned);
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
            registry: schemas
                .into_iter()
                .map(|s| (s.name.to_string(), s))
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
    let claims: Claims = serde_json::from_value(json!({
        "sub":"creator@example.gov", "roles":["member"], "perms":[], "exp":9999999999_u64,
        "tenant_chain":[{"schema":"Organization","entity_id":first.id}]
    }))
    .unwrap();
    let key = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(key.path(), [17; 32]).unwrap();
    let validator = PasetoAuth::new(&PasetoConfig {
        version: "v4".into(),
        purpose: "local".into(),
        key_path: key.path().into(),
        issuer: None,
        audience: None,
        public_paths: vec![],
    })
    .unwrap();
    let generator = Arc::new(PasetoGenerator::with_symmetric_key(
        [17; 32],
        TokenGenerationConfig::default(),
    ));
    let app = Router::new()
        .nest(
            "/forge",
            forge_routes()
                .merge(auth_routes())
                .layer(Extension(store.clone()))
                .layer(Extension(generator))
                .layer(Extension(Arc::new(PrincipalClaimMappings::default())))
                .layer(Extension(Arc::new(data.tenant_config.clone()))),
        )
        .layer(axum::middleware::from_fn_with_state(
            TenantScopeState {
                entity_store: backend,
                tenant_config: Arc::new(data.tenant_config.clone()),
            },
            tenant_scope::middleware,
        ))
        .with_state(service.state().clone());
    Fixture {
        app,
        store,
        claims,
        validator,
        _key: key,
    }
}
async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Value,
    claims: Claims,
    active: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(active) = active {
        req = req.header("x-active-tenant", active);
    }
    let mut req = req.body(Body::from(body.to_string())).unwrap();
    req.extensions_mut().insert(claims);
    let response = f.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test(flavor = "multi_thread")]
async fn root_creator_can_read_after_refresh_with_scoped_owner_role() {
    let f = fixture(Some("owner")).await;
    let (status, created) = request(
        &f,
        "POST",
        "/forge/schemas/Organization/entities",
        json!({"fields":{"name":"New","slug":"new"}}),
        f.claims.clone(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let active = format!("Organization:{id}");
    let path = format!("/forge/schemas/Organization/entities/{id}");
    assert_eq!(
        request(
            &f,
            "GET",
            &path,
            json!(null),
            f.claims.clone(),
            Some(&active)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, refresh) = request(
        &f,
        "POST",
        "/forge/auth/refresh",
        json!({}),
        f.claims.clone(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refresh}");
    let refreshed = f
        .validator
        .validate_token(refresh["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(refreshed.roles, vec!["member"]);
    assert_eq!(
        request(&f, "GET", &path, json!(null), refreshed, Some(&active))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.store
            .get_user("creator@example.gov")
            .await
            .unwrap()
            .unwrap()
            .roles,
        vec!["member"]
    );
    assert_eq!(
        f.store
            .list_tenant_memberships("creator@example.gov")
            .await
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn unset_creator_role_preserves_membership_behavior() {
    let f = fixture(None).await;
    let (status, created) = request(
        &f,
        "POST",
        "/forge/schemas/Organization/entities",
        json!({"fields":{"name":"New","slug":"new"}}),
        f.claims.clone(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        f.store
            .list_tenant_memberships("creator@example.gov")
            .await
            .unwrap()
            .len(),
        1
    );
}
