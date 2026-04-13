//! Integration tests for Phase 2a hook dispatch.
//!
//! These exercise the full route → actor → dispatcher path using a
//! [`MockHookDispatcher`]. The dispatcher's canned responses simulate
//! real hook behavior (modify, abort, timeout, unavailable) without
//! standing up an out-of-process gRPC server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use acton_service::config::Config;
use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_acton::hooks::{
    HookBinding, HookError, HookOutcome, HooksConfig, MockHookDispatcher,
};
use schema_forge_acton::messages::{
    ApplyMigration, InitForge, InsertSchema, ReplyChannel, StoreSchemaMetadata,
};
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::ForgeActor;
use schema_forge_core::migration::DiffEngine;
use schema_forge_core::types::{
    Annotation, DynamicValue, FieldDefinition, FieldModifier, FieldName, FieldType, HookEvent,
    SchemaDefinition, SchemaId, SchemaName, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use tokio::sync::oneshot;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

fn admin_claims() -> Claims {
    Claims {
        sub: "user:test-admin".to_string(),
        roles: vec!["admin".to_string()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    }
}

/// Build a `Translation` schema with `before_change` and `after_change`
/// hooks declared. The definition is identical to what a DSL parser
/// would produce for:
///
/// ```text
/// @hook(before_change) """patch fields"""
/// @hook(after_change) """publish event"""
/// schema Translation {
///     source_text: text required
///     translated_text: text
/// }
/// ```
fn translation_schema_with_hooks() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Translation").unwrap(),
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("source_text").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::new(
                FieldName::new("translated_text").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(FieldName::new("published_at").unwrap(), FieldType::DateTime),
        ],
        vec![
            Annotation::Hook {
                event: HookEvent::BeforeChange,
                intent: "patch fields".to_string(),
            },
            Annotation::Hook {
                event: HookEvent::AfterChange,
                intent: "publish event".to_string(),
            },
        ],
    )
    .unwrap()
}

/// Build a test app state with the given `HooksConfig` and dispatcher,
/// the `Translation` schema pre-registered in both the backend and the
/// in-memory registry.
async fn setup(
    hooks_config: HooksConfig,
    dispatcher: Option<Arc<MockHookDispatcher>>,
) -> AppState<SchemaForgeConfig> {
    use acton_service::service_builder::ServiceBuilder;

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    let mut custom = SchemaForgeConfig::default();
    custom.schema_forge.hooks = hooks_config;
    let config = Config {
        custom,
        ..Config::default()
    };

    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .build();

    let forge = service
        .state()
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // Initialize the actor with the backend and (optionally) the dispatcher.
    let (tx, rx) = oneshot::channel();
    forge
        .send(InitForge {
            registry: HashMap::new(),
            backend: Arc::new(backend),
            tenant_config: None,
            record_access_policy: None,
            hook_dispatcher: dispatcher
                .map(|d| d as Arc<dyn schema_forge_acton::hooks::HookDispatcher>),
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("InitForge timeout")
        .expect("InitForge channel dropped");

    // Create the Translation table in the backend and register the
    // annotated definition in the actor's registry.
    let schema = translation_schema_with_hooks();
    let plan = DiffEngine::create_new(&schema);

    let (tx, rx) = oneshot::channel();
    forge
        .send(ApplyMigration {
            schema_name: schema.name.clone(),
            steps: plan.steps,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("ApplyMigration timeout")
        .expect("ApplyMigration channel dropped")
        .expect("ApplyMigration failed");

    let (tx, rx) = oneshot::channel();
    forge
        .send(StoreSchemaMetadata {
            definition: schema.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("StoreSchemaMetadata timeout")
        .expect("StoreSchemaMetadata channel dropped")
        .expect("StoreSchemaMetadata failed");

    forge
        .send(InsertSchema {
            name: schema.name.as_str().to_string(),
            definition: schema,
        })
        .await;

    service.state().clone()
}

/// Wrap the forge router with an admin-claims middleware layer.
fn test_router(state: AppState<SchemaForgeConfig>) -> Router {
    let claims = admin_claims();
    forge_routes()
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let claims = claims.clone();
                async move {
                    req.extensions_mut().insert(claims);
                    next.run(req).await
                }
            },
        ))
        .with_state(state)
}

async fn post_entity(
    router: &Router,
    schema: &str,
    fields: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let body = serde_json::json!({ "fields": fields });
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/schemas/{schema}/entities"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn binding(required: bool, event: HookEvent) -> HookBinding {
    HookBinding {
        schema: "Translation".to_string(),
        event,
        endpoint: "http://mock".to_string(),
        timeout_ms: None,
        required,
        descriptor_path: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_change_hook_modifies_stored_fields() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    let mut modified = std::collections::BTreeMap::new();
    modified.insert(
        "translated_text".to_string(),
        DynamicValue::Text("¡hola!".to_string()),
    );
    dispatcher
        .respond_before(
            "Translation",
            HookEvent::BeforeChange,
            HookOutcome {
                abort_reason: None,
                modified_fields: Some(modified),
            },
        )
        .await;

    let config = HooksConfig {
        enabled: true,
        bindings: vec![binding(true, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher.clone())).await;
    let router = test_router(state);

    let (status, json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "response body: {json}");
    assert_eq!(json["fields"]["source_text"], "hello");
    assert_eq!(json["fields"]["translated_text"], "¡hola!");

    let calls = dispatcher.before_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].schema, "Translation");
    assert_eq!(calls[0].event, HookEvent::BeforeChange);
    assert_eq!(calls[0].operation, "create");
    assert_eq!(
        calls[0].fields.get("source_text"),
        Some(&DynamicValue::Text("hello".to_string()))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_change_hook_abort_returns_422() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    dispatcher
        .respond_before(
            "Translation",
            HookEvent::BeforeChange,
            HookOutcome {
                abort_reason: Some("profanity detected".to_string()),
                modified_fields: None,
            },
        )
        .await;

    let config = HooksConfig {
        enabled: true,
        bindings: vec![binding(false, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher)).await;
    let router = test_router(state);

    let (status, json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {json}");
    assert_eq!(json["error"], "hook_aborted");
    assert!(json["message"].as_str().unwrap().contains("profanity"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_hook_unavailable_returns_503() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    dispatcher
        .fail_before(
            "Translation",
            HookEvent::BeforeChange,
            HookError::Unavailable {
                endpoint: "http://mock".to_string(),
                message: "connection refused".to_string(),
            },
        )
        .await;

    let config = HooksConfig {
        enabled: true,
        bindings: vec![binding(true, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher)).await;
    let router = test_router(state);

    let (status, json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {json}");
    assert_eq!(json["error"], "hook_unavailable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn optional_hook_unavailable_proceeds() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    dispatcher
        .fail_before(
            "Translation",
            HookEvent::BeforeChange,
            HookError::Unavailable {
                endpoint: "http://mock".to_string(),
                message: "connection refused".to_string(),
            },
        )
        .await;

    let config = HooksConfig {
        enabled: true,
        bindings: vec![binding(false, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher)).await;
    let router = test_router(state);

    let (status, json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {json}");
    assert_eq!(json["fields"]["source_text"], "hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_change_hook_fires_without_blocking_response() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    let config = HooksConfig {
        enabled: true,
        bindings: vec![
            binding(true, HookEvent::BeforeChange),
            binding(false, HookEvent::AfterChange),
        ],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher.clone())).await;
    let router = test_router(state);

    let (status, _json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // after hooks are fire-and-forget via tokio::spawn; give the background
    // task a small window to run before asserting.
    for _ in 0..50 {
        if !dispatcher.after_calls().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let after = dispatcher.after_calls().await;
    assert_eq!(after.len(), 1, "after_change hook should have fired once");
    assert_eq!(after[0].event, HookEvent::AfterChange);
    assert_eq!(after[0].operation, "create");
    // After-hook sees the persisted entity id.
    assert!(after[0].entity_id.is_some());
}

/// Regression for issue #6: the hook dispatcher's merge step must coerce
/// datetime fields returned as RFC3339 strings (per the gRPC wire
/// contract in `docs/hooks-reference.md` §3.4) into `DynamicValue::DateTime`
/// before the Postgres backend binds them. Without this coercion, a
/// hook-stamped datetime is bound as text against a `timestamptz`
/// column and the write fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_change_hook_text_datetime_response_is_coerced() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    let mut modified = std::collections::BTreeMap::new();
    // The real proto decoder surfaces `datetime` proto strings as
    // `DynamicValue::Text`; MockHookDispatcher reproduces that shape.
    modified.insert(
        "published_at".to_string(),
        DynamicValue::Text("2025-04-12T10:00:00Z".to_string()),
    );
    dispatcher
        .respond_before(
            "Translation",
            HookEvent::BeforeChange,
            HookOutcome {
                abort_reason: None,
                modified_fields: Some(modified),
            },
        )
        .await;

    let config = HooksConfig {
        enabled: true,
        bindings: vec![binding(true, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher)).await;
    let router = test_router(state);

    let (status, json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "response body: {json}");
    // Backend persistence round-trips datetime as an RFC3339 string in
    // the JSON response; verify the exact value the hook stamped.
    let published = json["fields"]["published_at"].as_str().unwrap_or_default();
    assert!(
        !published.is_empty(),
        "published_at missing from response: {json}"
    );
    let parsed: chrono::DateTime<chrono::Utc> = published
        .parse()
        .unwrap_or_else(|e| panic!("published_at not RFC3339 ({published}): {e}"));
    let expected = "2025-04-12T10:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    assert_eq!(parsed, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_change_hook_invalid_datetime_returns_422() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    let mut modified = std::collections::BTreeMap::new();
    modified.insert(
        "published_at".to_string(),
        DynamicValue::Text("not-a-date".to_string()),
    );
    dispatcher
        .respond_before(
            "Translation",
            HookEvent::BeforeChange,
            HookOutcome {
                abort_reason: None,
                modified_fields: Some(modified),
            },
        )
        .await;

    let config = HooksConfig {
        enabled: true,
        bindings: vec![binding(true, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher)).await;
    let router = test_router(state);

    let (status, json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {json}");
    assert_eq!(json["error"], "hook_aborted");
    let message = json["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("published_at") && message.contains("invalid datetime"),
        "unexpected message: {message}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_not_invoked_when_disabled() {
    let dispatcher = Arc::new(MockHookDispatcher::new());
    let config = HooksConfig {
        enabled: false,
        bindings: vec![binding(true, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let state = setup(config, Some(dispatcher.clone())).await;
    let router = test_router(state);

    let (status, _json) = post_entity(
        &router,
        "Translation",
        serde_json::json!({"source_text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(dispatcher.before_calls().await.is_empty());
    assert!(dispatcher.after_calls().await.is_empty());
}
