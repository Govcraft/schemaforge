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
use schema_forge_acton::messages::{InitForge, ReplyChannel};
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::DynForgeBackend;
use schema_forge_acton::DynSchemaBackend;
use schema_forge_acton::ForgeActor;
use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::types::SchemaDefinition;
use schema_forge_surrealdb::SurrealBackend;
use tokio::sync::oneshot;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn make_test_claims(roles: &[&str]) -> Claims {
    Claims {
        sub: "user:test-user".to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
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

/// Parameters for building a test app with a ForgeActor.
struct TestForgeInit {
    backend: Arc<dyn DynForgeBackend>,
    registry: HashMap<String, SchemaDefinition>,
    tenant_config: Option<TenantConfig>,
    record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
    hook_dispatcher: Option<Arc<dyn schema_forge_acton::hooks::HookDispatcher>>,
}

/// Build a test `AppState<SchemaForgeConfig>` with a ForgeActor initialized from the given params.
///
/// Must be called from a multi-threaded tokio runtime (ServiceBuilder::build uses block_in_place).
async fn build_test_app_state(init: TestForgeInit) -> AppState<SchemaForgeConfig> {
    use acton_service::service_builder::ServiceBuilder;

    let config = Config::<SchemaForgeConfig>::default();
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .with_actor::<schema_forge_acton::HookDispatchActor>()
        .build();

    let forge_handle = service
        .state()
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    let (tx, rx) = oneshot::channel();
    forge_handle
        .send(InitForge {
            registry: init.registry,
            backend: init.backend,
            tenant_config: init.tenant_config,
            record_access_policy: init.record_access_policy,
            hook_dispatcher: init.hook_dispatcher,
            reply: ReplyChannel::new(tx),
        })
        .await;

    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("InitForge timeout")
        .expect("InitForge channel dropped");

    service.state().clone()
}

/// Create a simple test `AppState` with an empty in-memory SurrealDB backend.
async fn test_app_state() -> AppState<SchemaForgeConfig> {
    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    build_test_app_state(TestForgeInit {
        backend: Arc::new(backend),
        registry: HashMap::new(),
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await
}

/// Create a test router with an empty registry and admin Claims injected.
async fn test_app() -> Router {
    let state = test_app_state().await;
    test_app_with_claims_state(state, make_test_claims(&["admin"]))
}

/// Create a test router from an AppState without injecting Claims.
fn test_app_with_state(state: AppState<SchemaForgeConfig>) -> Router {
    forge_routes().with_state(state)
}

/// Create a test router that injects Claims into every request.
fn test_app_with_claims_state(state: AppState<SchemaForgeConfig>, claims: Claims) -> Router {
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

/// Helper to send a JSON request to the test app and get a response.
async fn json_request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let body = match body {
        Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
        None => Body::empty(),
    };

    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();

    let json = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null)
    };

    (status, json)
}

// ---------------------------------------------------------------------------
// Schema lifecycle tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_schema_returns_201() {
    let app = test_app().await;
    let body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "email", "field_type": "Text"},
            {"name": "age", "field_type": "Integer"}
        ]
    });

    let (status, json) = json_request(&app, Method::POST, "/schemas", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["name"], "Contact");
    assert_eq!(json["fields"].as_array().unwrap().len(), 3);
    assert!(json["id"].as_str().unwrap().starts_with("schema_"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_duplicate_schema_returns_409() {
    let app = test_app().await;
    let body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text"}
        ]
    });

    let (status, _) = json_request(&app, Method::POST, "/schemas", Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, json) = json_request(&app, Method::POST, "/schemas", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"], "schema_already_exists");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_existing_schema_returns_200() {
    let app = test_app().await;
    let body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text"}
        ]
    });

    json_request(&app, Method::POST, "/schemas", Some(body)).await;

    let (status, json) = json_request(&app, Method::GET, "/schemas/Contact", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["name"], "Contact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_missing_schema_returns_404() {
    let app = test_app().await;

    let (status, json) = json_request(&app, Method::GET, "/schemas/Missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"], "schema_not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_schemas_returns_all() {
    let app = test_app().await;

    // Create two schemas
    let body1 = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    let body2 = serde_json::json!({
        "name": "Company",
        "fields": [{"name": "title", "field_type": "Text"}]
    });

    json_request(&app, Method::POST, "/schemas", Some(body1)).await;
    json_request(&app, Method::POST, "/schemas", Some(body2)).await;

    let (status, json) = json_request(&app, Method::GET, "/schemas", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["count"], 2);
    assert_eq!(json["schemas"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_schema_triggers_migration() {
    let app = test_app().await;

    // Create schema
    let create_body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    json_request(&app, Method::POST, "/schemas", Some(create_body)).await;

    // Update schema with an additional field
    let update_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text"},
            {"name": "email", "field_type": "Text"}
        ]
    });
    let (status, json) =
        json_request(&app, Method::PUT, "/schemas/Contact", Some(update_body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["fields"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_schema_removes_from_registry() {
    let app = test_app().await;

    // Create schema
    let body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    json_request(&app, Method::POST, "/schemas", Some(body)).await;

    // Delete schema
    let (status, _) = json_request(&app, Method::DELETE, "/schemas/Contact", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Verify it is gone
    let (status, _) = json_request(&app, Method::GET, "/schemas/Contact", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Validation tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_schema_name_returns_400() {
    let app = test_app().await;
    let body = serde_json::json!({
        "name": "bad_name",
        "fields": [{"name": "name", "field_type": "Text"}]
    });

    let (status, json) = json_request(&app, Method::POST, "/schemas", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid_schema_name");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_fields_returns_422() {
    let app = test_app().await;
    let body = serde_json::json!({
        "name": "Contact",
        "fields": []
    });

    let (status, json) = json_request(&app, Method::POST, "/schemas", Some(body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json["error"], "validation_failed");
}

// ---------------------------------------------------------------------------
// Entity lifecycle tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_entity_returns_201() {
    let app = test_app().await;

    // Create schema first
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "age", "field_type": "Integer"}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // Create entity
    let entity_body = serde_json::json!({
        "fields": {
            "name": "Alice",
            "age": 30
        }
    });
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(entity_body),
    )
    .await;
    assert_eq!(
        (status, &json),
        (StatusCode::CREATED, &json),
        "expected 201, got {status} with body: {json}"
    );
    assert_eq!(json["schema"], "Contact");
    assert!(json["id"].as_str().unwrap().starts_with("entity_"));
    assert_eq!(json["fields"]["name"], "Alice");
    assert_eq!(json["fields"]["age"], 30);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_entity_for_missing_schema_returns_404() {
    let app = test_app().await;

    let entity_body = serde_json::json!({
        "fields": {
            "name": "Alice"
        }
    });
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Missing/entities",
        Some(entity_body),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"], "schema_not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_entity_returns_200() {
    let app = test_app().await;

    // Create schema
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // Create entity
    let entity_body = serde_json::json!({
        "fields": { "name": "Alice" }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(entity_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap();

    // Get entity
    let path = format!("/schemas/Contact/entities/{entity_id}");
    let (status, json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["id"], entity_id);
    assert_eq!(json["fields"]["name"], "Alice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_missing_entity_returns_404() {
    let app = test_app().await;

    // Create schema
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // Use a valid but non-existent entity ID
    let fake_id = schema_forge_core::types::EntityId::new();
    let path = format!("/schemas/Contact/entities/{}", fake_id.as_str());
    let (status, _) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_entity_returns_200() {
    let app = test_app().await;

    // Create schema
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "age", "field_type": "Integer"}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // Create entity
    let entity_body = serde_json::json!({
        "fields": { "name": "Alice", "age": 30 }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(entity_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap();

    // Update entity
    let update_body = serde_json::json!({
        "fields": { "name": "Alice Updated", "age": 31 }
    });
    let path = format!("/schemas/Contact/entities/{entity_id}");
    let (status, json) = json_request(&app, Method::PUT, &path, Some(update_body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["fields"]["name"], "Alice Updated");
    assert_eq!(json["fields"]["age"], 31);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_entity_returns_204() {
    let app = test_app().await;

    // Create schema
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text", "modifiers": ["required"]}]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // Create entity
    let entity_body = serde_json::json!({
        "fields": { "name": "Alice" }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(entity_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap();

    // Delete entity
    let path = format!("/schemas/Contact/entities/{entity_id}");
    let (status, _) = json_request(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Verify it is gone
    let (status, _) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_missing_entity_returns_404() {
    let app = test_app().await;

    // Create schema
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    let fake_id = schema_forge_core::types::EntityId::new();
    let path = format!("/schemas/Contact/entities/{}", fake_id.as_str());
    let (status, _) = json_request(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Cedar policy generation tests
// ---------------------------------------------------------------------------

#[test]
fn cedar_policies_generated_for_schema() {
    use schema_forge_acton::cedar::generate_cedar_policies;
    use schema_forge_core::types::{
        FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
        TextConstraints,
    };

    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Contact").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();

    let policies = generate_cedar_policies(&schema);
    assert_eq!(policies.len(), 4);
    assert!(policies[0].cedar_text.contains("ReadContact"));
    assert!(policies[1].cedar_text.contains("CreateContact"));
    assert!(policies[2].cedar_text.contains("DeleteContact"));
    assert!(policies[3].cedar_text.contains("UpdateSchema"));
}

// ---------------------------------------------------------------------------
// Extension builder tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_builder_with_backend_loads_schemas() {
    use schema_forge_acton::SchemaForgeExtension;

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    let builder = SchemaForgeExtension::builder().with_backend(backend);
    let extension = builder.build().await.expect("failed to build extension");

    // Registry should contain exactly the 5 system schemas after seeding
    let schemas = extension.registry().list().await;
    assert_eq!(schemas.len(), 5);
    let names: Vec<String> = schemas
        .iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"Permission".to_string()));
    assert!(names.contains(&"Role".to_string()));
    assert!(names.contains(&"User".to_string()));
    assert!(names.contains(&"TenantMembership".to_string()));
    assert!(names.contains(&"WebhookSubscription".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_register_routes_nests_under_forge() {
    // Build actor-backed AppState and nest forge routes under /forge
    let state = test_app_state().await;
    let router: Router = Router::new();
    let forge_router: Router<()> = forge_routes().with_state(state);
    let router = router.nest("/forge", forge_router);

    // Test that we can hit /forge/schemas (requires auth)
    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/forge/schemas")
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(make_test_claims(&["admin"]));

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Auth integration tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_with_admin_claims_succeeds() {
    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let state = build_test_app_state(TestForgeInit {
        backend: Arc::new(backend),
        registry: HashMap::new(),
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await;
    let app = test_app_with_claims_state(state, make_test_claims(&["admin"]));

    // Create a schema to verify the request goes through
    let body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    let (status, json) = json_request(&app, Method::POST, "/schemas", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["name"], "Contact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_without_claims_returns_401() {
    use schema_forge_core::types::{
        Annotation, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
        TextConstraints,
    };

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    // Register a schema so the route doesn't 404 before reaching auth check
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Contact").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Access {
            read: vec!["viewer".to_string()],
            write: vec!["editor".to_string()],
            delete: vec!["admin".to_string()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();

    let backend = Arc::new(backend);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("apply migration");
    backend
        .store_schema_metadata(&schema)
        .await
        .expect("store metadata");

    let mut registry = HashMap::new();
    registry.insert("Contact".to_string(), schema.clone());

    let state = build_test_app_state(TestForgeInit {
        backend,
        registry,
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await;
    // No Claims injected — requests should get 401
    let app = test_app_with_state(state);

    let (status, json) = json_request(&app, Method::GET, "/schemas/Contact/entities", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"], "unauthorized");
}

// ---------------------------------------------------------------------------
// Access control integration tests
// ---------------------------------------------------------------------------

/// Helper to create an AppState with a pre-registered schema with @access annotations,
/// and build a test app with the given Claims.
async fn access_test_state(
    user_roles: Vec<String>,
    schema_read_roles: Vec<String>,
    schema_write_roles: Vec<String>,
    schema_delete_roles: Vec<String>,
) -> (AppState<SchemaForgeConfig>, Router) {
    use schema_forge_core::types::{
        Annotation, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
        TextConstraints,
    };

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    // Create a schema with @access annotation
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Article").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("body").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
        ],
        vec![Annotation::Access {
            read: schema_read_roles,
            write: schema_write_roles,
            delete: schema_delete_roles,
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();

    let mut registry = HashMap::new();
    registry.insert("Article".to_string(), schema.clone());

    // Apply migration so the backend table exists
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    let backend = Arc::new(backend);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("failed to apply migration");
    backend
        .store_schema_metadata(&schema)
        .await
        .expect("failed to store metadata");

    let state = build_test_app_state(TestForgeInit {
        backend,
        registry,
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await;

    let claims = make_test_claims(&user_roles.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    let app = test_app_with_claims_state(state.clone(), claims);
    (state, app)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_request_with_matching_role_succeeds() {
    let (_state, app) = access_test_state(
        vec!["editor".to_string()],
        vec!["viewer".to_string(), "editor".to_string()],
        vec!["editor".to_string()],
        vec!["admin".to_string()],
    )
    .await;

    // Create entity -- user has "editor" role which is in write roles
    let entity_body = serde_json::json!({
        "fields": {
            "title": "Hello World",
            "body": "Content here"
        }
    });
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Article/entities",
        Some(entity_body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "expected 201, got {status} with body: {json}"
    );
    assert_eq!(json["schema"], "Article");
    assert_eq!(json["fields"]["title"], "Hello World");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_request_with_wrong_role_gets_403() {
    let (_state, app) = access_test_state(
        vec!["viewer".to_string()],
        vec!["viewer".to_string()],
        vec!["editor".to_string()],
        vec!["admin".to_string()],
    )
    .await;

    // Create entity -- user has "viewer" role, write requires "editor"
    let entity_body = serde_json::json!({
        "fields": {
            "title": "Forbidden Article",
            "body": "Should fail"
        }
    });
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Article/entities",
        Some(entity_body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"], "forbidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_without_claims_on_access_controlled_schema_returns_401() {
    use schema_forge_core::types::{
        Annotation, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
        TextConstraints,
    };

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    // Schema with restrictive @access
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Secret").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("data").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Access {
            read: vec!["classified".to_string()],
            write: vec!["classified".to_string()],
            delete: vec!["classified".to_string()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();

    let mut registry = HashMap::new();
    registry.insert("Secret".to_string(), schema.clone());

    let backend = Arc::new(backend);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("failed to apply migration");
    backend
        .store_schema_metadata(&schema)
        .await
        .expect("failed to store metadata");

    // No Claims injected — should get 401
    let state = build_test_app_state(TestForgeInit {
        backend,
        registry,
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await;
    let app = test_app_with_state(state);

    let entity_body = serde_json::json!({
        "fields": { "data": "top secret" }
    });
    let (status, _json) = json_request(
        &app,
        Method::POST,
        "/schemas/Secret/entities",
        Some(entity_body),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn field_filtering_hides_restricted_fields() {
    use schema_forge_core::types::{
        FieldAnnotation, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId,
        SchemaName, TextConstraints,
    };

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    // Schema with a field-level access restriction
    // @access with empty lists = all authenticated users permitted (testing field-level, not schema-level)
    use schema_forge_core::types::Annotation;
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Employee").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_annotations(
                FieldName::new("salary").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::FieldAccess {
                    read: vec!["hr".to_string()],
                    write: vec!["hr".to_string()],
                }],
            ),
        ],
        vec![Annotation::Access {
            read: vec![],
            write: vec![],
            delete: vec![],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();

    let mut registry = HashMap::new();
    registry.insert("Employee".to_string(), schema.clone());

    let backend = Arc::new(backend);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("failed to apply migration");
    backend
        .store_schema_metadata(&schema)
        .await
        .expect("failed to store metadata");

    // User with "member" role (not "hr")
    let state = build_test_app_state(TestForgeInit {
        backend,
        registry,
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await;
    let app = test_app_with_claims_state(state, make_test_claims(&["member"]));

    // Create entity with both fields
    let entity_body = serde_json::json!({
        "fields": {
            "name": "Alice",
            "salary": "100000"
        }
    });
    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Employee/entities",
        Some(entity_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The response should include "name" but NOT "salary" (filtered by read access)
    assert_eq!(json["fields"]["name"], "Alice");
    assert!(
        json["fields"].get("salary").is_none()
            || json["fields"]["salary"] == serde_json::Value::Null,
        "salary field should be filtered from response, got: {:?}",
        json["fields"]
    );

    // Get the entity back and verify salary is still filtered
    let entity_id = json["id"].as_str().unwrap();
    let path = format!("/schemas/Employee/entities/{entity_id}");
    let (status, get_json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(get_json["fields"]["name"], "Alice");
    assert!(
        get_json["fields"].get("salary").is_none()
            || get_json["fields"]["salary"] == serde_json::Value::Null,
        "salary field should be filtered from GET response, got: {:?}",
        get_json["fields"]
    );
}

// ---------------------------------------------------------------------------
// Issue #10 regression tests: GET → modify → PUT round-trip + PATCH support
// ---------------------------------------------------------------------------

/// Regression for issue #10: POST → GET → PUT (with the GET response body
/// echoed back unchanged) must succeed with 200. The pre-fix behavior
/// returned 502 for two unrelated reasons: GET emitted a `+00:00` datetime
/// suffix that the client could echo back fine but that downstream null
/// coercion and full-replacement validation tripped over; and the postgres
/// binder bound `null` fields as `text`, mismatching typed columns. This
/// test exercises the round-trip on the in-memory SurrealDB backend (which
/// shares the field-conversion code path with postgres), guaranteeing the
/// JSON serialization and required-field checks both accept the round-tripped
/// payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_round_trip_after_get_returns_200() {
    let app = test_app().await;

    // Schema: required name + optional updated_at datetime + optional notes.
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "updated_at", "field_type": "DateTime"},
            {"name": "notes", "field_type": "Text"}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // POST an entity that exercises a datetime field.
    let create_body = serde_json::json!({
        "fields": {
            "name": "Alice",
            "updated_at": "2026-04-13T12:00:00Z",
            "notes": "initial"
        }
    });
    let (create_status, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(create_body),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    let entity_id = created["id"].as_str().unwrap().to_string();
    let path = format!("/schemas/Contact/entities/{entity_id}");

    // GET the entity — capture its serialized fields verbatim.
    let (get_status, fetched) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(get_status, StatusCode::OK);
    let fetched_fields = fetched["fields"].clone();

    // The GET response must serialize datetimes with a `Z` suffix
    // (issue #10 fix #1) so generic clients consuming the format are happy.
    let serialized_dt = fetched_fields["updated_at"].as_str().unwrap();
    assert!(
        serialized_dt.ends_with('Z'),
        "expected datetime to end in 'Z', got {serialized_dt}"
    );

    // PUT the GET response body back unchanged. Pre-fix this returned 502;
    // post-fix it must return 200 because (a) the round-tripped datetime
    // re-parses cleanly and (b) the full payload satisfies the required-
    // field check.
    let put_body = serde_json::json!({ "fields": fetched_fields });
    let (put_status, put_json) = json_request(&app, Method::PUT, &path, Some(put_body)).await;
    assert_eq!(
        put_status,
        StatusCode::OK,
        "round-trip PUT should succeed, got {put_status} body={put_json}"
    );
    assert_eq!(put_json["fields"]["name"], "Alice");
}

/// Regression for issue #10: PATCH must merge a partial payload onto the
/// existing entity, preserving fields that are not mentioned in the
/// request body — including required ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_entity_merges_partial_fields() {
    let app = test_app().await;

    // Schema with two required fields plus one optional field. PATCH must
    // be able to update only `notes` without re-supplying `name` or `email`.
    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "email", "field_type": "Text", "modifiers": ["required"]},
            {"name": "notes", "field_type": "Text"}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    let create_body = serde_json::json!({
        "fields": {
            "name": "Alice",
            "email": "alice@example.com",
            "notes": "initial"
        }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(create_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap().to_string();
    let path = format!("/schemas/Contact/entities/{entity_id}");

    // Patch only the notes field.
    let patch_body = serde_json::json!({ "fields": { "notes": "patched" } });
    let (patch_status, patched) = json_request(&app, Method::PATCH, &path, Some(patch_body)).await;
    assert_eq!(
        patch_status,
        StatusCode::OK,
        "PATCH should succeed, got {patch_status} body={patched}"
    );

    // The unmodified required fields must still be present and unchanged.
    assert_eq!(patched["fields"]["name"], "Alice");
    assert_eq!(patched["fields"]["email"], "alice@example.com");
    assert_eq!(patched["fields"]["notes"], "patched");

    // Confirm via GET that the merge was actually persisted.
    let (_, fetched) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(fetched["fields"]["name"], "Alice");
    assert_eq!(fetched["fields"]["email"], "alice@example.com");
    assert_eq!(fetched["fields"]["notes"], "patched");
}

/// Regression for issue #12 and the partial-PATCH structural fix.
///
/// When PATCH only touches one field, the backend must receive an entity
/// containing *only* that delta — not the full merged row. This keeps the
/// backend's UPDATE narrow and structurally prevents the "null column gets
/// rebound with the wrong type" class of bugs (#12 was one instance of it
/// involving relation-many columns stored as text[]).
///
/// The observable surface of "narrow UPDATE" is that an unrelated field
/// that was never included in the patch body is still present and
/// unchanged after the round-trip. This test covers that semantic on a
/// schema that mixes scalar, array, and nullable fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_entity_narrow_update_preserves_untouched_nullable_fields() {
    let app = test_app().await;

    // Schema with a required scalar, a required scalar we will patch, and
    // two optional typed fields that stay null through the whole lifecycle.
    // DateTime is the salient case: its null binding used to fall through
    // to text before #10 landed typed-null binding.
    let schema_body = serde_json::json!({
        "name": "Task",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "duration_days", "field_type": "Float", "modifiers": ["required"]},
            {"name": "early_start", "field_type": "DateTime"},
            {"name": "total_slack", "field_type": "Float"}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    // Create without setting `early_start` or `total_slack` so they stay null.
    let create_body = serde_json::json!({
        "fields": {
            "name": "Discovery",
            "duration_days": 5.0
        }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Task/entities",
        Some(create_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap().to_string();
    let path = format!("/schemas/Task/entities/{entity_id}");

    // Patch only `duration_days`. Prior to the partial-PATCH fix, the
    // merged entity (including both null `early_start` and null
    // `total_slack`) would reach the backend and the null rebind would
    // fail on any backend whose null-binding path had a weak spot. With
    // the delta fix, the backend only ever sees `duration_days` in the
    // UPDATE.
    let patch_body = serde_json::json!({ "fields": { "duration_days": 7.5 } });
    let (patch_status, patched) = json_request(&app, Method::PATCH, &path, Some(patch_body)).await;
    assert_eq!(
        patch_status,
        StatusCode::OK,
        "PATCH should succeed even when unrelated columns are null, got {patch_status} body={patched}"
    );

    // Follow-up GET confirms the patched field landed and the untouched
    // nullable fields are still null.
    let (_, fetched) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(fetched["fields"]["name"], "Discovery");
    assert_eq!(fetched["fields"]["duration_days"], 7.5);
    assert!(
        fetched["fields"]["early_start"].is_null(),
        "early_start must still be null after PATCH, got {}",
        fetched["fields"]["early_start"]
    );
    assert!(
        fetched["fields"]["total_slack"].is_null(),
        "total_slack must still be null after PATCH, got {}",
        fetched["fields"]["total_slack"]
    );
}

/// A PATCH that re-sends fields identical to the current values must be
/// treated as a no-op: the backend does not need to be hit, and the
/// response is still the existing entity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_entity_noop_when_body_matches_existing_values() {
    let app = test_app().await;

    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "email", "field_type": "Text", "modifiers": ["required"]}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    let create_body = serde_json::json!({
        "fields": { "name": "Alice", "email": "alice@example.com" }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(create_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap().to_string();
    let path = format!("/schemas/Contact/entities/{entity_id}");

    // Patch with the exact same values the row already has.
    let patch_body = serde_json::json!({
        "fields": { "name": "Alice", "email": "alice@example.com" }
    });
    let (status, body) = json_request(&app, Method::PATCH, &path, Some(patch_body)).await;
    assert_eq!(status, StatusCode::OK, "no-op PATCH should succeed");
    assert_eq!(body["fields"]["name"], "Alice");
    assert_eq!(body["fields"]["email"], "alice@example.com");
}

/// Regression for issue #10: PUT with a missing required field must return
/// 422 and the error body must direct callers to PATCH for partial updates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_missing_required_field_suggests_patch() {
    let app = test_app().await;

    let schema_body = serde_json::json!({
        "name": "Contact",
        "fields": [
            {"name": "name", "field_type": "Text", "modifiers": ["required"]},
            {"name": "email", "field_type": "Text", "modifiers": ["required"]}
        ]
    });
    json_request(&app, Method::POST, "/schemas", Some(schema_body)).await;

    let create_body = serde_json::json!({
        "fields": { "name": "Alice", "email": "alice@example.com" }
    });
    let (_, created) = json_request(
        &app,
        Method::POST,
        "/schemas/Contact/entities",
        Some(create_body),
    )
    .await;
    let entity_id = created["id"].as_str().unwrap().to_string();
    let path = format!("/schemas/Contact/entities/{entity_id}");

    // PUT with only the `name` field — `email` is missing.
    let put_body = serde_json::json!({ "fields": { "name": "Alice Updated" } });
    let (status, body) = json_request(&app, Method::PUT, &path, Some(put_body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The error message must include the actionable PATCH hint.
    let message = body["message"].as_str().unwrap_or("");
    assert!(
        message.contains("PATCH"),
        "error message should mention PATCH for partial updates, got: {message}"
    );
    assert!(
        message.contains("email"),
        "error message should name the missing field, got: {message}"
    );
}

// ---------------------------------------------------------------------------
// Derived inverse relation tests (issue #34)
// ---------------------------------------------------------------------------

/// Builds a (paired) pair of schemas: `Opportunity` with a parent collection
/// `documents: -> Document[]` and `Document` with `opportunity: -> Opportunity`.
/// Applies migrations and stores metadata so the backend has live tables.
async fn setup_paired_schemas() -> (AppState<SchemaForgeConfig>, Router) {
    use schema_forge_core::types::{
        Cardinality, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaDefinition,
        SchemaId, SchemaName, TextConstraints,
    };

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let backend = Arc::new(backend);

    let mut opportunity = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Opportunity").unwrap(),
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::new(
                FieldName::new("documents").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Document").unwrap(),
                    cardinality: Cardinality::Many,
                },
            ),
        ],
        vec![],
    )
    .unwrap();

    let mut document = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Document").unwrap(),
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::new(
                FieldName::new("opportunity").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Opportunity").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
        ],
        vec![],
    )
    .unwrap();

    // Run the inverse pairing pass to mark Opportunity.documents as derived.
    let mut batch = vec![opportunity.clone(), document.clone()];
    schema_forge_core::inverse_relations::pair_inverse_relations(&mut batch).unwrap();
    opportunity = batch[0].clone();
    document = batch[1].clone();

    assert!(
        opportunity.field("documents").unwrap().is_derived(),
        "pairing should have marked Opportunity.documents as derived"
    );

    // Apply migrations & store metadata for both schemas. The migration
    // plan for Opportunity should NOT create a documents column.
    for schema in &[&opportunity, &document] {
        let plan = schema_forge_core::migration::DiffEngine::create_new(schema);
        backend
            .apply_migration(&schema.name, &plan.steps)
            .await
            .expect("apply migration");
        backend
            .store_schema_metadata(schema)
            .await
            .expect("store metadata");
    }

    let mut registry = HashMap::new();
    registry.insert("Opportunity".to_string(), opportunity);
    registry.insert("Document".to_string(), document);

    let state = build_test_app_state(TestForgeInit {
        backend,
        registry,
        tenant_config: None,
        record_access_policy: None,
        hook_dispatcher: None,
    })
    .await;
    let app = test_app_with_claims_state(state.clone(), make_test_claims(&["admin"]));
    (state, app)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn derived_collection_populated_on_single_get() {
    let (_state, app) = setup_paired_schemas().await;

    // Create an opportunity.
    let (status, opp) = json_request(
        &app,
        Method::POST,
        "/schemas/Opportunity/entities",
        Some(serde_json::json!({ "fields": { "title": "Deal A" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let opp_id = opp["id"].as_str().unwrap().to_string();

    // Create three documents referencing the opportunity.
    let mut doc_ids = Vec::new();
    for title in &["Doc 1", "Doc 2", "Doc 3"] {
        let (status, doc) = json_request(
            &app,
            Method::POST,
            "/schemas/Document/entities",
            Some(serde_json::json!({
                "fields": { "title": title, "opportunity": opp_id }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        doc_ids.push(doc["id"].as_str().unwrap().to_string());
    }

    // GET the opportunity and verify the derived collection is populated.
    let (status, body) = json_request(
        &app,
        Method::GET,
        &format!("/schemas/Opportunity/entities/{opp_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let documents = body["fields"]["documents"]
        .as_array()
        .expect("documents should be an array, not null");
    let returned: Vec<String> = documents
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(returned.len(), 3, "expected three child document IDs");
    for id in &doc_ids {
        assert!(
            returned.contains(id),
            "missing child id {id} in {returned:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn derived_collection_populated_on_list() {
    let (_state, app) = setup_paired_schemas().await;

    // Two parents, two children each.
    let mut parent_ids = Vec::new();
    for title in &["Deal A", "Deal B"] {
        let (_status, opp) = json_request(
            &app,
            Method::POST,
            "/schemas/Opportunity/entities",
            Some(serde_json::json!({ "fields": { "title": title } })),
        )
        .await;
        parent_ids.push(opp["id"].as_str().unwrap().to_string());
    }
    for (i, pid) in parent_ids.iter().enumerate() {
        for n in 0..2 {
            json_request(
                &app,
                Method::POST,
                "/schemas/Document/entities",
                Some(serde_json::json!({
                    "fields": { "title": format!("p{i}-doc{n}"), "opportunity": pid }
                })),
            )
            .await;
        }
    }

    let (status, body) =
        json_request(&app, Method::GET, "/schemas/Opportunity/entities", None).await;
    assert_eq!(status, StatusCode::OK);
    let entities = body["entities"].as_array().unwrap();
    assert_eq!(entities.len(), 2);
    for entity in entities {
        let docs = entity["fields"]["documents"].as_array().unwrap();
        assert_eq!(docs.len(), 2, "each parent should see its two children");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_to_derived_field_is_rejected() {
    let (_state, app) = setup_paired_schemas().await;

    // Attempt to create an Opportunity with a value in the derived field.
    let (status, body) = json_request(
        &app,
        Method::POST,
        "/schemas/Opportunity/entities",
        Some(serde_json::json!({
            "fields": {
                "title": "Deal A",
                "documents": ["entity_01foo"]
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let message = body["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or("");
    assert!(
        message.to_lowercase().contains("derived")
            || body.to_string().to_lowercase().contains("derived"),
        "error should mention 'derived': {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_with_no_children_gets_empty_array_not_null() {
    let (_state, app) = setup_paired_schemas().await;

    let (_status, opp) = json_request(
        &app,
        Method::POST,
        "/schemas/Opportunity/entities",
        Some(serde_json::json!({ "fields": { "title": "Lonely Deal" } })),
    )
    .await;
    let opp_id = opp["id"].as_str().unwrap().to_string();

    let (status, body) = json_request(
        &app,
        Method::GET,
        &format!("/schemas/Opportunity/entities/{opp_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let documents = &body["fields"]["documents"];
    assert!(
        documents.is_array(),
        "documents should always be an array, got {documents}"
    );
    assert_eq!(documents.as_array().unwrap().len(), 0);
}
