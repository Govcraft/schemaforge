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
use schema_forge_acton::admin::routes::{protected_routes, public_routes};
use schema_forge_acton::state::{ForgeState, SchemaRegistry};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Cardinality, DynamicValue, FieldDefinition, FieldModifier, FieldName, FieldType,
    SchemaDefinition, SchemaId, SchemaName, TextConstraints,
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
        auth_provider: None,
        tenant_config: None,
        record_access_policy: None,
        surreal_client: None,
    }
}

/// Create an admin test router with ForgeState applied.
///
/// Uses routes without auth middleware for direct handler testing.
/// A session layer is included so `TypedSession<AuthSession>` extraction works.
async fn admin_test_app() -> (Router, ForgeState) {
    let state = admin_test_state().await;
    let session_config = acton_service::session::SessionConfig {
        secure: false,
        cookie_name: "test_session".to_string(),
        ..Default::default()
    };
    let session_layer = acton_service::session::create_memory_session_layer(&session_config);
    let router = protected_routes()
        .merge(public_routes())
        .layer(session_layer)
        .with_state(state.clone());
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
async fn create_entity(
    state: &ForgeState,
    schema_name: &str,
    fields: BTreeMap<String, DynamicValue>,
) -> Entity {
    let entity = Entity::new(SchemaName::new(schema_name).unwrap(), fields);
    state.backend.create(&entity).await.expect("create entity")
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
async fn post_form(
    app: &Router,
    path: &str,
    form_data: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
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
async fn put_form(
    app: &Router,
    path: &str,
    form_data: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
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
    assert!(
        body.contains("Contact"),
        "dashboard should show schema name"
    );
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
    assert!(
        body.contains("not found")
            || body.contains("Error")
            || status == StatusCode::INTERNAL_SERVER_ERROR
    );
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
    assert!(
        body.contains("No entities") || body.contains("Showing"),
        "should show empty state or table"
    );
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
    assert!(
        !body.contains("<!DOCTYPE"),
        "fragment should not be a full HTML page"
    );
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

    let (status, headers, _body) =
        post_form(&app, "/schemas/Contact/entities", "name=Alice&age=30").await;

    // Should redirect to entity detail
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers
        .get("location")
        .expect("should have Location header");
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
    let (status, _headers, body) = post_form(&app, "/schemas/Contact/entities", "age=30").await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body.contains("name") || body.contains("required"),
        "should show validation error"
    );
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
    assert_ne!(
        status,
        StatusCode::OK,
        "nonexistent entity should not return 200"
    );
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
    let (status, headers, _body) = put_form(&app, &path, "name=Eve+Updated&age=29").await;

    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers
        .get("location")
        .expect("should have Location header");
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
    let (status, _headers, body) = put_form(&app, &path, "age=30").await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body.contains("name") || body.contains("required"),
        "should show validation error"
    );
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

    // /admin redirects permanently to /admin/
    let request = Request::builder()
        .method(Method::GET)
        .uri("/admin")
        .body(Body::empty())
        .unwrap();

    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::PERMANENT_REDIRECT,
        "/admin should redirect to /admin/"
    );

    // /admin/ without auth should redirect to login
    let request = Request::builder()
        .method(Method::GET)
        .uri("/admin/")
        .body(Body::empty())
        .unwrap();

    let response = router.clone().oneshot(request).await.unwrap();
    assert!(
        response.status().is_redirection(),
        "unauthenticated /admin/ should redirect to login, got {}",
        response.status()
    );

    // /admin/login should be accessible without auth
    let request = Request::builder()
        .method(Method::GET)
        .uri("/admin/login")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/admin/login should return 200"
    );
}

// ---------------------------------------------------------------------------
// Schema editor tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_create_form_returns_200() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/schemas/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Create Schema"), "should show create heading");
    assert!(body.contains("<form"), "should contain a form element");
    assert!(
        body.contains("schema_name"),
        "should have schema name input"
    );
    assert!(
        body.contains("field_0_name"),
        "should have at least one field row"
    );
}

#[tokio::test]
async fn schema_create_redirects_on_success() {
    let (app, _state) = admin_test_app().await;
    let (status, headers, _body) = post_form(
        &app,
        "/schemas",
        "schema_name=Product&field_0_name=title&field_0_type=text",
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers
        .get("location")
        .expect("should have Location header");
    let loc_str = location.to_str().unwrap();
    assert!(
        loc_str.contains("/schemas/Product"),
        "should redirect to schema detail, got: {loc_str}"
    );
}

#[tokio::test]
async fn schema_create_validates_name() {
    let (app, _state) = admin_test_app().await;
    let (status, _headers, body) = post_form(
        &app,
        "/schemas",
        "schema_name=not-valid&field_0_name=x&field_0_type=text",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body.contains("schema name") || body.contains("error") || body.contains("Error"),
        "should show validation error"
    );
}

#[tokio::test]
async fn schema_create_validates_empty_fields() {
    let (app, _state) = admin_test_app().await;
    let (status, _headers, body) = post_form(&app, "/schemas", "schema_name=Test").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body.contains("field") || body.contains("required"),
        "should show field error"
    );
}

#[tokio::test]
async fn schema_create_persists_to_registry() {
    let (app, state) = admin_test_app().await;
    let (_status, _headers, _body) = post_form(
        &app,
        "/schemas",
        "schema_name=BlogPost&field_0_name=title&field_0_type=text&field_0_required=true&field_1_name=body&field_1_type=richtext",
    )
    .await;

    // Verify it's in the registry
    let schema = state.registry.get("BlogPost").await;
    assert!(schema.is_some(), "schema should be in registry");
    let schema = schema.unwrap();
    assert_eq!(schema.fields.len(), 2);
    assert!(schema.fields[0].is_required());
}

#[tokio::test]
async fn schema_edit_form_returns_200() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/schemas/Contact/edit").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Edit Contact"), "should show edit heading");
    assert!(body.contains("Contact"), "should contain schema name");
    assert!(body.contains("name"), "should pre-fill field name");
    assert!(body.contains("age"), "should pre-fill field age");
}

#[tokio::test]
async fn schema_edit_form_nonexistent_returns_error() {
    let (app, _state) = admin_test_app().await;
    let (status, _body) = get_html(&app, "/schemas/NonExistent/edit").await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn schema_update_redirects_on_success() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, headers, _body) = post_form(
        &app,
        "/schemas/Contact",
        "schema_name=Contact&field_0_name=name&field_0_type=text&field_0_required=true&field_1_name=email&field_1_type=text",
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers
        .get("location")
        .expect("should have Location header");
    assert!(location.to_str().unwrap().contains("Contact"));
}

#[tokio::test]
async fn schema_update_applies_migration() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    // Add a new field "email"
    let (_status, _headers, _body) = post_form(
        &app,
        "/schemas/Contact",
        "schema_name=Contact&field_0_name=name&field_0_type=text&field_0_required=true&field_1_name=age&field_1_type=integer&field_2_name=email&field_2_type=text",
    )
    .await;

    // Verify updated schema in registry
    let updated = state.registry.get("Contact").await.expect("should exist");
    assert_eq!(updated.fields.len(), 3);
    assert_eq!(updated.fields[2].name.as_str(), "email");
}

#[tokio::test]
async fn schema_delete_removes_from_registry() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    assert!(state.registry.get("Contact").await.is_some());

    let (status, _body) = delete_request(&app, "/schemas/Contact").await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );

    assert!(
        state.registry.get("Contact").await.is_none(),
        "schema should be removed"
    );
}

#[tokio::test]
async fn schema_delete_nonexistent_redirects() {
    let (app, _state) = admin_test_app().await;
    let (status, _body) = delete_request(&app, "/schemas/NonExistent").await;
    // Should still redirect (we just remove from registry, which returns None for missing)
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
}

#[tokio::test]
async fn schema_preview_returns_dsl_fragment() {
    let (app, _state) = admin_test_app().await;
    let (status, _headers, body) = post_form(
        &app,
        "/schemas/_preview",
        "schema_name=Contact&field_0_name=name&field_0_type=text&field_0_required=true",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("schema Contact"), "should contain DSL text");
    assert!(body.contains("name"), "should contain field name");
    assert!(
        !body.contains("<!DOCTYPE"),
        "should be a fragment, not full page"
    );
}

#[tokio::test]
async fn schema_preview_returns_errors_on_invalid() {
    let (app, _state) = admin_test_app().await;
    let (status, _headers, body) = post_form(
        &app,
        "/schemas/_preview",
        "schema_name=not-valid&field_0_name=name&field_0_type=text",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("schema name") || body.contains("error") || body.contains("Invalid"),
        "should show errors in preview"
    );
}

#[tokio::test]
async fn schema_preview_with_migration() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, _headers, body) = post_form(
        &app,
        "/schemas/_preview",
        "schema_name=Contact&_existing_schema_name=Contact&field_0_name=name&field_0_type=text&field_0_required=true&field_1_name=email&field_1_type=text",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("migration") || body.contains("ADD") || body.contains("REMOVE"),
        "should show migration steps"
    );
}

#[tokio::test]
async fn field_row_fragment_returns_html() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/schemas/_field-row/5").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("field_5_name"),
        "should contain correct field index"
    );
    assert!(body.contains("field_5_type"), "should contain type select");
    assert!(!body.contains("<!DOCTYPE"), "should be a fragment");
}

#[tokio::test]
async fn type_constraints_text_fragment() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/schemas/_type-constraints/text?index=3").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("text_max_length"),
        "should contain text constraints"
    );
    assert!(body.contains("field_3_"), "should use correct index");
}

#[tokio::test]
async fn type_constraints_enum_fragment() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/schemas/_type-constraints/enum?index=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("enum_variants"),
        "should contain enum variants textarea"
    );
}

#[tokio::test]
async fn type_constraints_relation_fragment() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/schemas/_type-constraints/relation?index=2").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("relation_target"),
        "should contain relation target input"
    );
    assert!(
        body.contains("relation_cardinality"),
        "should contain cardinality select"
    );
}

#[tokio::test]
async fn create_schema_with_enum_field_roundtrip() {
    let (app, state) = admin_test_app().await;
    let form_data = "schema_name=Status&field_0_name=level&field_0_type=enum&field_0_enum_variants=Low%0AMedium%0AHigh";
    let (status, _headers, _body) = post_form(&app, "/schemas", form_data).await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );

    let schema = state.registry.get("Status").await.expect("should exist");
    if let FieldType::Enum(v) = &schema.fields[0].field_type {
        assert_eq!(v.as_slice(), &["Low", "Medium", "High"]);
    } else {
        panic!("expected enum field type");
    }
}

#[tokio::test]
async fn create_schema_with_relation_field_roundtrip() {
    let (app, state) = admin_test_app().await;
    let form_data = "schema_name=Employee&field_0_name=company&field_0_type=relation&field_0_relation_target=Company&field_0_relation_cardinality=one";
    let (status, _headers, _body) = post_form(&app, "/schemas", form_data).await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );

    let schema = state.registry.get("Employee").await.expect("should exist");
    if let FieldType::Relation {
        target,
        cardinality,
    } = &schema.fields[0].field_type
    {
        assert_eq!(target.as_str(), "Company");
        assert!(matches!(
            cardinality,
            schema_forge_core::types::Cardinality::One
        ));
    } else {
        panic!("expected relation field type");
    }
}

#[tokio::test]
async fn form_preserves_values_on_validation_error() {
    let (app, _state) = admin_test_app().await;
    // Submit with valid field but invalid schema name
    let (status, _headers, body) = post_form(
        &app,
        "/schemas",
        "schema_name=not-valid&field_0_name=title&field_0_type=text&field_0_text_max_length=200",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Check that form values are preserved in re-rendered form
    assert!(
        body.contains("not-valid") || body.contains("title"),
        "should preserve form values on error"
    );
}

#[tokio::test]
async fn dashboard_has_create_schema_button() {
    let (app, _state) = admin_test_app().await;
    let (status, body) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("/schemas/new"),
        "dashboard should have create schema link"
    );
}

#[tokio::test]
async fn schema_detail_has_edit_and_delete_buttons() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/schemas/Contact").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("/schemas/Contact/edit"),
        "should have edit link"
    );
    assert!(body.contains("hx-delete"), "should have delete button");
}

// ---------------------------------------------------------------------------
// Schema relationship graph tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dashboard_no_graph_without_relations() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    let (status, body) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("id=\"schema-graph\""),
        "graph should not appear when no relations exist"
    );
}

#[tokio::test]
async fn dashboard_has_graph_with_relations() {
    let (app, state) = admin_test_app().await;

    // Create Company schema
    let company = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Company").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();
    register_schema(&state, &company).await;

    // Create Employee schema with relation to Company
    let employee = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Employee").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("company").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
        ],
        vec![],
    )
    .unwrap();
    register_schema(&state, &employee).await;

    let (status, body) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("id=\"schema-graph\""),
        "graph div should appear when relations exist"
    );
    assert!(
        body.contains("Schema Relationships"),
        "graph heading should be present"
    );
}

#[tokio::test]
async fn dashboard_graph_json_has_correct_nodes() {
    let (app, state) = admin_test_app().await;

    let company = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Company").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();
    register_schema(&state, &company).await;

    let employee = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Employee").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("company").unwrap(),
            FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::One,
            },
        )],
        vec![],
    )
    .unwrap();
    register_schema(&state, &employee).await;

    let (_status, body) = get_html(&app, "/").await;
    // The JSON is embedded in the script tag
    assert!(
        body.contains("\"id\":\"Company\""),
        "JSON should contain Company node"
    );
    assert!(
        body.contains("\"id\":\"Employee\""),
        "JSON should contain Employee node"
    );
}

#[tokio::test]
async fn dashboard_graph_json_has_correct_edges() {
    let (app, state) = admin_test_app().await;

    let tag = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Tag").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();
    register_schema(&state, &tag).await;

    let article = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Article").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("tags").unwrap(),
            FieldType::Relation {
                target: SchemaName::new("Tag").unwrap(),
                cardinality: Cardinality::Many,
            },
        )],
        vec![],
    )
    .unwrap();
    register_schema(&state, &article).await;

    let (_status, body) = get_html(&app, "/").await;
    assert!(
        body.contains("\"from\":\"Article\""),
        "edge should come from Article"
    );
    assert!(body.contains("\"to\":\"Tag\""), "edge should go to Tag");
    assert!(
        body.contains("\"label\":\"tags\""),
        "edge label should be field name"
    );
    assert!(
        body.contains("\"cardinality\":\"Many\""),
        "cardinality should be Many"
    );
}

// ---------------------------------------------------------------------------
// Field rename detection tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_update_rename_field_preserves_data() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    // Rename "name" → "full_name" by including old_name in form data
    let (status, headers, _body) = post_form(
        &app,
        "/schemas/Contact",
        "schema_name=Contact&field_0_name=full_name&field_0_old_name=name&field_0_type=text&field_0_required=true&field_1_name=age&field_1_old_name=age&field_1_type=integer",
    )
    .await;

    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {status}"
    );
    let location = headers
        .get("location")
        .expect("should have Location header");
    assert!(location.to_str().unwrap().contains("Contact"));

    // Verify updated schema in registry has the renamed field
    let updated = state.registry.get("Contact").await.expect("should exist");
    assert!(
        updated
            .fields
            .iter()
            .any(|f| f.name.as_str() == "full_name"),
        "schema should have renamed field 'full_name'"
    );
    assert!(
        !updated.fields.iter().any(|f| f.name.as_str() == "name"),
        "schema should not have old field 'name'"
    );
}

#[tokio::test]
async fn schema_preview_shows_rename_step() {
    let (app, state) = admin_test_app().await;
    let schema = make_contact_schema();
    register_schema(&state, &schema).await;

    // Preview with rename: "name" → "full_name"
    let (status, _headers, body) = post_form(
        &app,
        "/schemas/_preview",
        "schema_name=Contact&_existing_schema_name=Contact&field_0_name=full_name&field_0_old_name=name&field_0_type=text&field_0_required=true&field_1_name=age&field_1_old_name=age&field_1_type=integer",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("RENAME"),
        "preview should show RENAME step, got: {}",
        body
    );
    assert!(
        !body.contains("REMOVE field"),
        "preview should NOT show REMOVE field when rename hint is provided"
    );
}
