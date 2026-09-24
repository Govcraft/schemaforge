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
use schema_forge_backend::SchemaBackend;
use schema_forge_core::migration::DiffEngine;
use schema_forge_surrealdb::SurrealBackend;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower::ServiceExt;

async fn app() -> Router {
    let backend = SurrealBackend::connect_with_auth("mem://", "graphql", "writes", None, None)
        .await
        .unwrap();
    let schemas = schema_forge_dsl::parse(
        r#"
        @access(read: ["staff", "manager"], write: ["staff", "manager"])
        schema Line {
            label: text required
            number: text @field_access(read: ["staff", "manager"], write: ["manager"])
            status: text required @default("'pending'")
                @require("status != 'live' || number != null", "live needs number")
            has_number: boolean required @compute("number != null")
            enabled: boolean required default(true)
        }
    "#,
    )
    .unwrap();
    for schema in &schemas {
        backend
            .apply_migration(&schema.name, &DiffEngine::create_new(schema))
            .await
            .unwrap();
        backend.store_schema_metadata(schema).await.unwrap();
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
    extension
        .register_graphql_routes(Router::new())
        .with_state(service.state().clone())
}

async fn query(app: &Router, query: &str, role: &str) -> Value {
    let claims = Claims {
        sub: "user:graphql-test".into(),
        roles: vec![role.into()],
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
    let mut request = Request::post("/forge/graphql")
        .header("content-type", "application/json")
        .body(Body::from(json!({"query": query}).to_string()))
        .unwrap();
    request.extensions_mut().insert(claims);
    let response = app.clone().oneshot(request).await.unwrap();
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn graphql_writes_share_defaults_authorization_rules_and_noop_semantics() {
    let app = app().await;
    let created = query(&app, r#"mutation { createLine(input: {label: "test", number: "denied"}) { id status has_number enabled number } }"#, "staff").await;
    assert!(created.get("errors").is_none(), "{created}");
    let row = &created["data"]["createLine"];
    assert_eq!(row["status"], "pending");
    assert_eq!(row["has_number"], false);
    assert_eq!(row["enabled"], true);
    assert!(row["number"].is_null());
    let id = row["id"].as_str().unwrap();

    let rejected = query(&app, r#"mutation { createLine(input: {label: "invalid", status: "live", number: "denied"}) { id } }"#, "staff").await;
    assert!(rejected.get("errors").is_some(), "{rejected}");
    let noop = query(&app, &format!(r#"mutation {{ updateLine(id: "{id}", input: {{number: "denied"}}) {{ id number has_number }} }}"#), "staff").await;
    assert!(noop.get("errors").is_none(), "{noop}");
    assert_eq!(noop["data"]["updateLine"]["has_number"], false);
    let rejected = query(&app, &format!(r#"mutation {{ updateLine(id: "{id}", input: {{status: "live", number: "denied"}}) {{ id }} }}"#), "staff").await;
    assert!(rejected.get("errors").is_some(), "{rejected}");

    let authorized = query(&app, &format!(r#"mutation {{ updateLine(id: "{id}", input: {{status: "live", number: "allowed"}}) {{ number has_number status }} }}"#), "manager").await;
    assert!(authorized.get("errors").is_none(), "{authorized}");
    assert_eq!(authorized["data"]["updateLine"]["has_number"], true);
    assert_eq!(authorized["data"]["updateLine"]["number"], "allowed");
}
