//! Schema mutations must honor the actor's custom policy contract before DDL.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    authz::{PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks},
    config::SchemaForgeConfig,
    messages::{ApplyPreparedSchemaChange, GetSchema, InitForge, ReplyChannel},
    routes::forge_routes,
    ForgeActor,
};
use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::{
    migration::DiffEngine,
    types::{DynamicValue, FieldDefinition, FieldModifier, FieldName, FieldType, TextConstraints},
};
use schema_forge_surrealdb::SurrealBackend;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower::ServiceExt;

async fn request(app: &Router, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
    let claims = Claims {
        sub: "user:policy-admin".into(),
        roles: vec!["platform_admin".into()],
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
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(claims);
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_policy_field_contract_refuses_mutations_before_storage_changes() {
    let schema = schema_forge_dsl::parse("schema Thing { code: text required label: text }")
        .unwrap()
        .remove(0);
    let backend = Arc::new(
        SurrealBackend::connect_memory("policy", "preflight")
            .await
            .unwrap(),
    );
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let entity = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("code".into(), DynamicValue::Text("keep".into())),
            ("label".into(), DynamicValue::Text("untouched".into())),
        ]),
    );
    backend.create(&entity).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("contract.cedar"),
        r#"forbid (principal is Forge::Principal, action == Action::"ReadThing", resource is Thing) when { resource.code == "secret" };"#).unwrap();
    let snapshot = PolicyStoreSnapshot::from_schemas(
        std::slice::from_ref(&schema),
        Some(directory.path()),
        RoleRanks::empty(),
        PrincipalClaimMappings::default(),
    )
    .unwrap();
    let policy_hash = snapshot.policy_hash.clone();
    let policies = Arc::new(PolicyStore::new(snapshot));
    // Config has no custom directory: the actor path models a CLI override.
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
            registry: HashMap::from([("Thing".into(), schema.clone())]),
            backend: backend.clone(),
            tenant_config: None,
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: Default::default(),
            policy_store: Some(policies.clone()),
            custom_policies_dir: Some(directory.path().to_path_buf()),
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    let app = forge_routes().with_state(service.state().clone());
    let drop_field = json!({"name":"Thing", "allow_destructive_migrations":true, "fields":[{"name":"label","field_type":"Text"}]});
    let rename_field = json!({"name":"Thing", "allow_destructive_migrations":true, "fields":[
        {"name":"renamed","field_type":"Text","modifiers":["required"],"annotations":[{"annotation":"RenamedFrom","name":"code"}]},
        {"name":"label","field_type":"Text"}
    ]});
    for (method, body) in [
        (Method::PUT, drop_field),
        (Method::PUT, rename_field),
        (Method::DELETE, Value::Null),
    ] {
        let (status, result) = request(&app, method, "/schemas/Thing", body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{result}");
        assert!(
            result
                .to_string()
                .contains("Cedar policy validation failed"),
            "{result}"
        );
        assert_eq!(
            backend
                .load_schema_metadata(&schema.name)
                .await
                .unwrap()
                .unwrap(),
            schema
        );
        assert_eq!(
            backend.get(&schema.name, &entity.id).await.unwrap().fields,
            entity.fields
        );
        assert_eq!(policies.current().policy_hash, policy_hash);
        let (status, result) = request(&app, Method::GET, "/schemas/Thing", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert!(result["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["name"] == "code"));
    }
    // Prepare two competing changes from the same immutable registry/bundle.
    let mut desired = schema.clone();
    desired.fields.push(FieldDefinition::new(
        FieldName::new("extra").unwrap(),
        FieldType::Text(TextConstraints::default()),
    ));
    let mut competing = schema.clone();
    competing
        .fields
        .retain(|field| field.name.as_str() != "label");
    let compile = |definition: &schema_forge_core::types::SchemaDefinition| {
        Arc::new(
            PolicyStoreSnapshot::from_schemas(
                std::slice::from_ref(definition),
                Some(directory.path()),
                RoleRanks::empty(),
                PrincipalClaimMappings::default(),
            )
            .unwrap(),
        )
    };
    let desired_policy = compile(&desired);
    let competing_policy = compile(&competing);
    let expected_policy = policies.current();
    let expected_registry = HashMap::from([("Thing".into(), schema.clone())]);
    // If commit rereads the source, this would fail after already changing DDL.
    std::fs::write(directory.path().join("contract.cedar"), "invalid policy").unwrap();
    let forge = service.state().actor::<ForgeActor>().unwrap();
    let (tx, rx) = oneshot::channel();
    forge
        .send(ApplyPreparedSchemaChange {
            expected_registry: expected_registry.clone(),
            expected_policy: expected_policy.clone(),
            next_policy: desired_policy.clone(),
            definition: desired.clone(),
            remove: false,
            steps: DiffEngine::plan_update(&schema, &desired).unwrap().steps,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&policies.current(), &desired_policy));
    assert_eq!(
        backend
            .load_schema_metadata(&schema.name)
            .await
            .unwrap()
            .unwrap(),
        desired
    );
    let (tx, rx) = oneshot::channel();
    forge
        .send(ApplyPreparedSchemaChange {
            expected_registry,
            expected_policy,
            next_policy: competing_policy,
            definition: competing.clone(),
            remove: false,
            steps: DiffEngine::plan_update(&schema, &competing).unwrap().steps,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let error = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error,
        schema_forge_acton::error::ForgeError::Conflict {
            reason: "schema_preflight_stale",
            ..
        }
    ));
    assert_eq!(
        backend.get(&schema.name, &entity.id).await.unwrap().fields["label"],
        DynamicValue::Text("untouched".into())
    );
    assert_eq!(
        backend
            .load_schema_metadata(&schema.name)
            .await
            .unwrap()
            .unwrap(),
        desired
    );

    // A storage rejection restores the provisional registry before replying.
    std::fs::write(directory.path().join("contract.cedar"),
        r#"forbid (principal is Forge::Principal, action == Action::"ReadThing", resource is Thing) when { resource.code == "secret" };"#).unwrap();
    backend
        .create(&Entity::new(schema.name.clone(), entity.fields.clone()))
        .await
        .unwrap();
    let mut invalid = desired.clone();
    invalid
        .fields
        .iter_mut()
        .find(|field| field.name.as_str() == "code")
        .unwrap()
        .modifiers
        .push(FieldModifier::Unique);
    let (tx, rx) = oneshot::channel();
    forge
        .send(ApplyPreparedSchemaChange {
            expected_registry: HashMap::from([("Thing".into(), desired.clone())]),
            expected_policy: policies.current(),
            next_policy: compile(&invalid),
            definition: invalid.clone(),
            remove: false,
            steps: DiffEngine::plan_update(&desired, &invalid).unwrap().steps,
            reply: ReplyChannel::new(tx),
        })
        .await;
    assert!(tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(Arc::ptr_eq(&policies.current(), &desired_policy));
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: "Thing".into(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        desired
    );
    assert_eq!(
        backend
            .load_schema_metadata(&schema.name)
            .await
            .unwrap()
            .unwrap(),
        desired
    );
}
