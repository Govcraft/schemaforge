#![cfg(feature = "graphql")]

use std::{collections::HashMap, sync::Arc, time::Duration};

use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use schema_forge_acton::{
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    ForgeActor, HookDispatchActor, SchemaForgeExtension,
};
use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::migration::DiffEngine;
use schema_forge_core::types::{DynamicValue, SchemaName};
use schema_forge_surrealdb::SurrealBackend;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::oneshot;
use tower::ServiceExt;

async fn app() -> (Router, String, String) {
    let backend = SurrealBackend::connect_with_auth("mem://", "graphql", "tenants", None, None)
        .await
        .unwrap();
    let schemas = schema_forge_dsl::parse(
        r#"
        @tenant(root)
        @access(read: ["member"], write: ["member"], delete: ["member"])
        schema Org { name: text required }
        @access(read: ["member"], write: ["member"])
        schema Catalog { name: text required }
        "#,
    )
    .unwrap();
    for schema in &schemas {
        backend
            .apply_migration(&schema.name, &DiffEngine::create_new(schema).steps)
            .await
            .unwrap();
        backend.store_schema_metadata(schema).await.unwrap();
    }
    // Direct storage seeds reproduce pre-fix tenant roots with NULL metadata.
    let mut roots = Vec::new();
    for name in ["a", "b"] {
        let entity = Entity::new(
            SchemaName::new("Org").unwrap(),
            BTreeMap::from([("name".into(), DynamicValue::Text(name.into()))]),
        );
        roots.push(entity.id.to_string());
        EntityStore::create(&backend, &entity).await.unwrap();
    }
    let extension = SchemaForgeExtension::builder()
        .with_backend(backend)
        .build()
        .await
        .unwrap();
    let forge_state = extension.state();
    let service = ServiceBuilder::new()
        .with_config(Config::<SchemaForgeConfig>::default())
        .with_actor::<ForgeActor>()
        .with_actor::<HookDispatchActor>()
        .build();
    let (tx, rx) = oneshot::channel();
    service
        .state()
        .actor::<ForgeActor>()
        .unwrap()
        .send(InitForge {
            registry: forge_state
                .registry
                .list()
                .await
                .into_iter()
                .map(|s| (s.name.as_str().to_owned(), s))
                .collect(),
            backend: forge_state.backend.clone(),
            tenant_config: forge_state.tenant_config.clone(),
            record_access_policy: forge_state.record_access_policy.clone(),
            hook_dispatcher: None,
            storage_registry: forge_state.storage_registry.clone(),
            policy_store: Some(Arc::clone(&forge_state.policy_store)),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap();
    let app = extension
        .register_graphql_routes(Router::new())
        .with_state(service.state().clone());
    (app, roots.remove(0), roots.remove(0))
}

async fn query(app: &Router, query: &str, tenant: &str) -> Value {
    let claims = Claims {
        sub: "user:graphql-test".into(),
        roles: vec!["member".into()],
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
            json!([{"schema":"Org","entity_id":tenant}]),
        )]),
    };
    let mut request = Request::post("/forge/graphql")
        .header("content-type", "application/json")
        .body(Body::from(json!({"query": query}).to_string()))
        .unwrap();
    request.extensions_mut().insert(claims);
    let response = app.clone().oneshot(request).await.unwrap();
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graphql_scopes_legacy_roots_and_keeps_shared_catalog_usable() {
    let (app, own, foreign) = app().await;
    let get = query(
        &app,
        &format!("{{ org(id: \"{own}\") {{ id name }} }}"),
        &own,
    )
    .await;
    assert_eq!(get["data"]["org"]["id"], own, "{get}");
    let denied = query(
        &app,
        &format!("{{ org(id: \"{foreign}\") {{ id name }} }}"),
        &own,
    )
    .await;
    assert!(denied.get("errors").is_some(), "{denied}");
    let list = query(&app, "{ orgs { items { id } totalCount } }", &own).await;
    assert_eq!(
        list["data"]["orgs"]["items"].as_array().unwrap().len(),
        1,
        "{list}"
    );
    assert!(list["data"]["orgs"]["totalCount"].is_null());
    let denied = query(
        &app,
        &format!("mutation {{ deleteOrg(id: \"{foreign}\") }}"),
        &own,
    )
    .await;
    assert!(denied.get("errors").is_some(), "{denied}");
    let created = query(
        &app,
        "mutation { createCatalog(input: {name: \"shared\"}) { id name } }",
        &own,
    )
    .await;
    assert!(created.get("errors").is_none(), "{created}");
    let catalog = query(&app, "{ catalogs { items { name } } }", &foreign).await;
    assert_eq!(
        catalog["data"]["catalogs"]["items"][0]["name"], "shared",
        "{catalog}"
    );
}
