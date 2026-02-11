//! Integration tests for the admin UI handlers.
//!
//! These tests exercise the admin HTML routes with a real in-memory SurrealDB backend.
//! They verify status codes, HTML content, form submissions, and HTMX fragment responses.

#![cfg(feature = "admin-ui")]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use schema_forge_acton::admin::routes::admin_routes;
use schema_forge_acton::state::{ForgeState, SchemaRegistry};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    DynamicValue, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaDefinition, SchemaId,
    SchemaName, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a test ForgeState with in-memory SurrealDB.
async fn admin_test_state() -> ForgeState {
    let backend = SurrealBackend::connect_memory("test", "admin_test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let registry = SchemaRegistry::new();
    ForgeState {
        registry,
        backend: Arc::new(backend),
    }
}

/// Create an admin test router with ForgeState applied.
async fn admin_test_app() -> (Router, ForgeState) {
    let state = admin_test_state().await;
    let router = admin_routes().with_state(state.clone());
    (router, state)
}

/// Helper to build a simple schema definition with text + integer fields.
fn make_contact_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Contact").unwrap(),
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            ),
            FieldDefinition::new(
                FieldName::new("age").unwrap(),
                FieldType::Integer(Default::default()),
            ),
        ],
        vec![],
    )
    .unwrap()
}

/// Register a schema in the backend and registry.
async fn register_schema(state: &ForgeState, schema: &SchemaDefinition) {
    state
        .backend
        .store_schema_metadata(schema)
        .await
        .expect("store schema metadata");
    state
        .backend
        .apply_migration(
            &schema.name,
            &schema_forge_core::migration::DiffEngine::create_new(schema).steps,
        )
        .await
        .expect("apply migration");
    state
        .registry
        .insert(schema.name.as_str().to_string(), schema.clone())
        .await;
}

/// Create an entity in the backend.
async fn create_entity(state: &ForgeState, schema_name: &str, fields: BTreeMap<String, DynamicValue>) -> Entity {
    let entity = Entity::new(SchemaName::new(schema_name).unwrap(), fields);
    state
        .backend
        .create(&entity)
        .await
        .expect("create entity")
}

/// Send a GET request and return (status, body string).
async fn get_html(app: &Router, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body_bytes.to_vec()).unwrap();
    (status, body)
}

/// Send a POST form request and return (status, headers, body string).
async fn post_form(app: &Router, path: &str, form_data: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form_data.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body_bytes.to_vec()).unwrap();
    (status, headers, body)
}

/// Send a PUT form request and return (status, headers, body string).
async fn put_form(app: &Router, path: &str, form_data: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let request = Request::builder()
        .method(Method::PUT)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form_data.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body_bytes.to_vec()).unwrap();
    (status, headers, body)
}

/// Send a DELETE request and return (status, body string).
async fn delete_request(app: &Router, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(Method::DELETE)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body_bytes.to_vec()).unwrap();
    (status, body)
}

// ---------------------------------------------------------------------------
// Dashboard tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dashboard_returns_200_empty() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("SchemaForge Admin"));
    assert!(body.contains("Dashboard"));
}

#[tokio::test]
async fn dashboard_lists_registered_schemas() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Contact"), "dashboard should show schema name");
}

// ---------------------------------------------------------------------------
// Schema detail tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_detail_returns_200() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/schemas/Contact").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Contact"), "should show schema name");
    assert!(body.contains("name"), "should show field name");
    assert!(body.contains("age"), "should show field age");
}

#[tokio::test]
async fn schema_detail_nonexistent_returns_error() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/schemas/NonExistent").await;
    // AdminError returns HTML error page (500 for internal or custom status)
    assert_ne!(status, StatusCode::OK);
    assert!(body.contains("not found") || body.contains("Error") || status == StatusCode::INTERNAL_SERVER_ERROR);
}

// ---------------------------------------------------------------------------
// Entity list tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_list_returns_200_empty() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/schemas/Contact/entities").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Contact"), "should show schema name");
    assert!(body.contains("No entities") || body.contains("Showing"), "should show empty state or table");
}

#[tokio::test]
async fn entity_list_shows_entities() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Alice".into()));
    fields.insert("age".to_string(), DynamicValue::Integer(30));
    create_entity(&state, "Contact", fields).await;

    let (status, body) = get_html(&app, "/schemas/Contact/entities").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Alice"), "should show entity data");
}

#[tokio::test]
async fn entity_list_with_pagination_params() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    // Create a few entities
    for i in 0..3 {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text(format!("Person{i}")));
        create_entity(&state, "Contact", fields).await;
    }

    let (status, body) = get_html(&app, "/schemas/Contact/entities?limit=2&offset=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Showing"), "should show pagination info");
}

// ---------------------------------------------------------------------------
// Entity table fragment (HTMX partial) tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_table_fragment_returns_html() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Bob".into()));
    create_entity(&state, "Contact", fields).await;

    let (status, body) = get_html(&app, "/schemas/Contact/entities/_table").await;
    assert_eq!(status, StatusCode::OK);
    // Fragment should contain table rows, not full page layout
    assert!(body.contains("Bob"), "fragment should contain entity data");
    assert!(!body.contains("<!DOCTYPE"), "fragment should not be a full HTML page");
}

// ---------------------------------------------------------------------------
// Entity create form tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_create_form_returns_200() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/schemas/Contact/entities/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Create"), "should show create heading");
    assert!(body.contains("name"), "form should contain field name");
    assert!(body.contains("age"), "form should contain field age");
    assert!(body.contains("<form"), "should contain a form element");
}

// ---------------------------------------------------------------------------
// Entity create (POST) tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_create_redirects_on_success() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, headers, _body) = post_form(
        &app,
        "/schemas/Contact/entities",
        "name=Alice&age=30",
    )
    .await;

    // Should redirect to entity detail
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers.get("location").expect("should have Location header");
    let loc_str = location.to_str().unwrap();
    assert!(
        loc_str.starts_with("/admin/schemas/Contact/entities/entity_"),
        "should redirect to entity detail, got: {loc_str}"
    );
}

#[tokio::test]
async fn entity_create_validates_required_fields() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    // Submit without required "name" field
    let (status, _headers, body) = post_form(
        &app,
        "/schemas/Contact/entities",
        "age=30",
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("name") || body.contains("required"), "should show validation error");
}

// ---------------------------------------------------------------------------
// Entity detail tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_detail_returns_200() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Carol".into()));
    fields.insert("age".to_string(), DynamicValue::Integer(25));
    let entity = create_entity(&state, "Contact", fields).await;

    let path = format!("/schemas/Contact/entities/{}", entity.id.as_str());
    let (status, body) = get_html(&app, &path).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Carol"), "should show entity name");
    assert!(body.contains("25"), "should show entity age");
}

#[tokio::test]
async fn entity_detail_nonexistent_returns_error() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let fake_id = schema_forge_core::types::EntityId::new();
    let path = format!("/schemas/Contact/entities/{}", fake_id.as_str());
    let (status, _body) = get_html(&app, &path).await;
    assert_ne!(status, StatusCode::OK, "nonexistent entity should not return 200");
}

// ---------------------------------------------------------------------------
// Entity edit form tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_edit_form_returns_200_with_values() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Dave".into()));
    fields.insert("age".to_string(), DynamicValue::Integer(40));
    let entity = create_entity(&state, "Contact", fields).await;

    let path = format!("/schemas/Contact/entities/{}/edit", entity.id.as_str());
    let (status, body) = get_html(&app, &path).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Edit"), "should show edit heading");
    assert!(body.contains("Dave"), "should pre-fill name value");
    assert!(body.contains("40"), "should pre-fill age value");
    assert!(body.contains("<form"), "should contain a form element");
}

// ---------------------------------------------------------------------------
// Entity update (PUT) tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_update_redirects_on_success() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Eve".into()));
    fields.insert("age".to_string(), DynamicValue::Integer(28));
    let entity = create_entity(&state, "Contact", fields).await;
    let eid = entity.id.as_str().to_string();

    let path = format!("/schemas/Contact/entities/{eid}");
    let (status, headers, _body) = put_form(
        &app,
        &path,
        "name=Eve+Updated&age=29",
    )
    .await;

    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers.get("location").expect("should have Location header");
    let loc_str = location.to_str().unwrap();
    assert!(
        loc_str.contains(&eid),
        "should redirect to the same entity, got: {loc_str}"
    );
}

#[tokio::test]
async fn entity_update_validates_required_fields() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Frank".into()));
    let entity = create_entity(&state, "Contact", fields).await;

    let path = format!("/schemas/Contact/entities/{}", entity.id.as_str());
    let (status, _headers, body) = put_form(
        &app,
        &path,
        "age=30",
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("name") || body.contains("required"), "should show validation error");
}

// ---------------------------------------------------------------------------
// Entity delete tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entity_delete_returns_ok() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Grace".into()));
    let entity = create_entity(&state, "Contact", fields).await;

    let path = format!("/schemas/Contact/entities/{}", entity.id.as_str());
    let (status, _body) = delete_request(&app, &path).await;
    assert_eq!(status, StatusCode::OK, "delete should return 200 for HTMX");
}

#[tokio::test]
async fn entity_delete_removes_entity() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Heidi".into()));
    let entity = create_entity(&state, "Contact", fields).await;

    let path = format!("/schemas/Contact/entities/{}", entity.id.as_str());
    let (status, _body) = delete_request(&app, &path).await;
    assert_eq!(status, StatusCode::OK);

    // Verify entity is gone — detail should return error
    let (status, _body) = get_html(&app, &path).await;
    assert_ne!(status, StatusCode::OK, "entity should be deleted");
}

// ---------------------------------------------------------------------------
// Relation options tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn relation_options_returns_html_fragment() {
    let (app, state) = admin_test_app().await;

    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    // Create some entities to appear as options
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), DynamicValue::Text("Ivan".into()));
    create_entity(&state, "Contact", fields).await;

    let (status, body) = get_html(&app, "/schemas/Contact/relation-options/contact_id").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<option"), "should return option elements");
    assert!(body.contains("Ivan"), "should contain entity display value");
}

// ---------------------------------------------------------------------------
// Sidebar navigation tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sidebar_contains_all_schemas() {
    let (app, state) = admin_test_app().await;

    let contact = make_contact_schema();
    register_schema(&state, &contact).await;

    let product = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Product").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("title").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();
    register_schema(&state, &product).await;

    let (status, body) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Contact"), "sidebar should list Contact");
    assert!(body.contains("Product"), "sidebar should list Product");
}

// ---------------------------------------------------------------------------
// Extension integration test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_admin_routes_mounts_under_admin() {
    use schema_forge_acton::SchemaForgeExtension;

    let backend = SurrealBackend::connect_memory("test", "ext_admin_test")
        .await
        .expect("in-memory backend");

    let extension = SchemaForgeExtension::builder()
        .with_backend(backend)
        .build()
        .await
        .expect("extension");

    let router: Router = Router::new();
    let router = extension.register_admin_routes(router);

    // Try both /admin and /admin/ — axum nest may or may not add trailing slash
    let request = Request::builder()
        .method(Method::GET)
        .uri("/admin")
        .body(Body::empty())
        .unwrap();

    let response = router.clone().oneshot(request).await.unwrap();
    let status_no_slash = response.status();

    let request = Request::builder()
        .method(Method::GET)
        .uri("/admin/")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    let status_with_slash = response.status();

    assert!(
        status_no_slash == StatusCode::OK || status_with_slash == StatusCode::OK,
        "expected 200 from /admin or /admin/, got {status_no_slash} and {status_with_slash}"
    );
}
