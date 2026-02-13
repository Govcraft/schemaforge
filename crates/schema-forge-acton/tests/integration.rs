use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::{DynSchemaBackend, ForgeState, SchemaRegistry};
use schema_forge_surrealdb::SurrealBackend;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "graphql")]
fn test_graphql_schema() -> Arc<arc_swap::ArcSwap<async_graphql::dynamic::Schema>> {
    use async_graphql::dynamic::{Field, FieldFuture, FieldValue, Object, Schema, TypeRef};
    let query = Object::new("Query").field(Field::new(
        "_empty",
        TypeRef::named(TypeRef::BOOLEAN),
        |_ctx| FieldFuture::new(async { Ok(None::<FieldValue>) }),
    ));
    let schema = Schema::build("Query", None, None)
        .register(query)
        .finish()
        .expect("test GraphQL schema");
    Arc::new(arc_swap::ArcSwap::new(Arc::new(schema)))
}

/// Create a test ForgeState with in-memory SurrealDB.
async fn test_state() -> ForgeState {
    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let registry = SchemaRegistry::new();
    ForgeState {
        registry,
        backend: Arc::new(backend),
        auth_provider: None,
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    }
}

/// Create a test router with ForgeState and auth middleware applied.
async fn test_app() -> Router {
    let state = test_state().await;
    test_app_with_state(state)
}

/// Create a test router with a specific ForgeState and auth middleware applied.
fn test_app_with_state(state: ForgeState) -> Router {
    forge_routes()
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            schema_forge_acton::middleware::auth_middleware,
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
async fn get_missing_schema_returns_404() {
    let app = test_app().await;

    let (status, json) = json_request(&app, Method::GET, "/schemas/Missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"], "schema_not_found");
}

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
async fn extension_builder_with_backend_loads_schemas() {
    use schema_forge_acton::SchemaForgeExtension;

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    let extension = SchemaForgeExtension::builder()
        .with_backend(backend)
        .build()
        .await
        .expect("failed to build extension");

    // Registry should contain exactly the 4 system schemas after seeding
    let schemas = extension.registry().list().await;
    assert_eq!(schemas.len(), 4);
    let names: Vec<String> = schemas
        .iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"Permission".to_string()));
    assert!(names.contains(&"Role".to_string()));
    assert!(names.contains(&"User".to_string()));
    assert!(names.contains(&"TenantMembership".to_string()));
}

#[tokio::test]
async fn extension_register_routes_nests_under_forge() {
    use schema_forge_acton::SchemaForgeExtension;

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");

    let extension = SchemaForgeExtension::builder()
        .with_backend(backend)
        .build()
        .await
        .expect("failed to build extension");

    let router: Router = Router::new();
    let router = extension.register_routes(router);

    // Test that we can hit /forge/schemas
    let request = Request::builder()
        .method(Method::GET)
        .uri("/forge/schemas")
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Auth middleware integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_with_noop_auth_succeeds() {
    use schema_forge_acton::auth::NoopAuthProvider;

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let registry = SchemaRegistry::new();
    let state = ForgeState {
        registry,
        backend: Arc::new(backend),
        auth_provider: Some(Arc::new(NoopAuthProvider::admin())),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    };
    let app = test_app_with_state(state);

    // Create a schema to verify the request goes through
    let body = serde_json::json!({
        "name": "Contact",
        "fields": [{"name": "name", "field_type": "Text"}]
    });
    let (status, json) = json_request(&app, Method::POST, "/schemas", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["name"], "Contact");
}

#[tokio::test]
async fn request_with_failing_auth_returns_401() {
    use std::future::Future;
    use std::pin::Pin;

    use schema_forge_acton::auth::AuthProvider;
    use schema_forge_backend::auth::{AuthContext, AuthError};

    /// An auth provider that always fails with MissingCredentials.
    struct FailingAuthProvider;

    impl AuthProvider for FailingAuthProvider {
        fn authenticate<'a>(
            &'a self,
            _parts: &'a axum::http::request::Parts,
        ) -> Pin<Box<dyn Future<Output = Result<AuthContext, AuthError>> + Send + 'a>> {
            Box::pin(async { Err(AuthError::MissingCredentials) })
        }
    }

    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let registry = SchemaRegistry::new();
    let state = ForgeState {
        registry,
        backend: Arc::new(backend),
        auth_provider: Some(Arc::new(FailingAuthProvider)),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(&app, Method::GET, "/schemas", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"], "unauthorized");
}

// ---------------------------------------------------------------------------
// Access control integration tests
// ---------------------------------------------------------------------------

/// Helper to create a ForgeState with a NoopAuthProvider configured with specific roles,
/// and a pre-registered schema with @access annotations.
async fn access_test_state(
    user_roles: Vec<String>,
    schema_read_roles: Vec<String>,
    schema_write_roles: Vec<String>,
    schema_delete_roles: Vec<String>,
) -> (ForgeState, Router) {
    use schema_forge_acton::auth::NoopAuthProvider;
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

    let registry = SchemaRegistry::new();
    registry.insert("Article".to_string(), schema.clone()).await;

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

    let state = ForgeState {
        registry,
        backend,
        auth_provider: Some(Arc::new(NoopAuthProvider::new(user_roles))),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    };
    let app = test_app_with_state(state.clone());
    (state, app)
}

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
async fn open_access_request_always_succeeds() {
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

    let registry = SchemaRegistry::new();
    registry.insert("Secret".to_string(), schema.clone()).await;

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

    // No auth_provider => open access mode
    let state = ForgeState {
        registry,
        backend,
        auth_provider: None,
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    };
    let app = test_app_with_state(state);

    // Even with restrictive @access, open access should succeed
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
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
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

    let registry = SchemaRegistry::new();
    registry
        .insert("Employee".to_string(), schema.clone())
        .await;

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
    use schema_forge_acton::auth::NoopAuthProvider;
    let state = ForgeState {
        registry,
        backend,
        auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["member".to_string()]))),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    };
    let app = test_app_with_state(state);

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
