use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::{ForgeState, SchemaRegistry};
use schema_forge_surrealdb::SurrealBackend;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a test ForgeState with in-memory SurrealDB.
async fn test_state() -> ForgeState {
    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let registry = SchemaRegistry::new();
    ForgeState {
        registry,
        backend: Arc::new(backend),
        #[cfg(feature = "admin-ui")]
        surreal_client: None,
    }
}

/// Create a test router with ForgeState applied.
async fn test_app() -> Router {
    let state = test_state().await;
    forge_routes().with_state(state)
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

    // Registry should be empty since no schemas exist yet
    let schemas = extension.registry().list().await;
    assert!(schemas.is_empty());
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
