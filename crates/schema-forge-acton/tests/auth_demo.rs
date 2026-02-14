//! End-to-end demonstration of all authorization & multi-tenancy features.
//!
//! This test exercises every feature from Phases 1-6:
//! - System schemas seeded at startup (Phase 2)
//! - AuthContext & AuthProvider (Phase 3)
//! - Schema-level @access enforcement (Phase 4)
//! - Field-level @field_access filtering (Phase 4)
//! - Multi-tenancy with @tenant annotations (Phase 5)
//! - Record-level @owner enforcement (Phase 6)

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use schema_forge_acton::auth::{AuthProvider, NoopAuthProvider};
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::{DynForgeBackend, ForgeState, SchemaRegistry};
use schema_forge_backend::auth::{AuthContext, AuthError, OwnershipBasedPolicy, TenantRef};
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::migration::DiffEngine;
use schema_forge_core::types::{
    Annotation, EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType, SchemaDefinition,
    SchemaId, SchemaName, TenantKind, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Helpers
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

#[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
fn test_theme() -> Arc<arc_swap::ArcSwap<schema_forge_acton::theme::Theme>> {
    Arc::new(arc_swap::ArcSwap::new(Arc::new(
        schema_forge_acton::theme::Theme::default(),
    )))
}

fn test_app_with_state(state: ForgeState) -> Router {
    forge_routes()
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            schema_forge_acton::middleware::auth_middleware,
        ))
        .with_state(state)
}

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

/// Auth provider that returns a configurable AuthContext.
struct ConfigurableAuthProvider {
    user_id: EntityId,
    roles: Vec<String>,
    tenant_chain: Vec<TenantRef>,
}

impl AuthProvider for ConfigurableAuthProvider {
    fn authenticate<'a>(
        &'a self,
        _parts: &'a axum::http::request::Parts,
    ) -> Pin<Box<dyn Future<Output = Result<AuthContext, AuthError>> + Send + 'a>> {
        let ctx = AuthContext {
            user_id: self.user_id.clone(),
            roles: self.roles.clone(),
            tenant_chain: self.tenant_chain.clone(),
            attributes: BTreeMap::new(),
        };
        Box::pin(async move { Ok(ctx) })
    }
}

/// Register a schema directly into the backend + registry.
async fn register_schema(
    schema: &SchemaDefinition,
    backend: &Arc<dyn DynForgeBackend>,
    registry: &SchemaRegistry,
) {
    let plan = DiffEngine::create_new(schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("apply migration");
    backend
        .store_schema_metadata(schema)
        .await
        .expect("store metadata");
    registry
        .insert(schema.name.as_str().to_string(), schema.clone())
        .await;
}

// ===========================================================================
// TEST 1: System schemas seeded at startup (Phase 2)
// ===========================================================================

#[tokio::test]
async fn demo_system_schemas_seeded_at_startup() {
    println!("\n=== DEMO: System Schemas Seeded at Startup ===");

    let backend = SurrealBackend::connect_memory("test", "demo_system")
        .await
        .unwrap();
    let extension = schema_forge_acton::SchemaForgeExtension::builder()
        .with_backend(backend)
        .build()
        .await
        .expect("extension build");

    let schemas = extension.registry().list().await;
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();

    println!("  System schemas found: {:?}", names);
    assert!(names.contains(&"User"), "User schema should be seeded");
    assert!(names.contains(&"Role"), "Role schema should be seeded");
    assert!(
        names.contains(&"Permission"),
        "Permission schema should be seeded"
    );
    assert!(
        names.contains(&"TenantMembership"),
        "TenantMembership schema should be seeded"
    );
    assert!(names.contains(&"Theme"), "Theme schema should be seeded");
    assert_eq!(schemas.len(), 5);

    // Verify system schemas are protected from deletion
    let user_schema = schemas.iter().find(|s| s.name.as_str() == "User").unwrap();
    assert!(
        user_schema.is_system(),
        "User schema should have @system annotation"
    );

    // Verify DiffEngine rejects dropping system schemas
    let result = DiffEngine::validate_system_schema_protection(
        user_schema,
        &schema_forge_core::migration::MigrationPlan {
            id: schema_forge_core::migration::MigrationId::new(),
            schema_id: user_schema.id.clone(),
            schema_name: user_schema.name.clone(),
            steps: vec![schema_forge_core::migration::MigrationStep::DropSchema {
                name: SchemaName::new("User").unwrap(),
            }],
        },
    );
    assert!(
        result.is_err(),
        "Should reject DropSchema on @system schema"
    );
    println!("  System schema deletion protection: VERIFIED");
    println!("  PASSED\n");
}

// ===========================================================================
// TEST 2: Schema-level @access enforcement (Phase 4)
// ===========================================================================

#[tokio::test]
async fn demo_schema_access_control() {
    println!("\n=== DEMO: Schema-Level @access Enforcement ===");

    let backend = SurrealBackend::connect_memory("test", "demo_access")
        .await
        .unwrap();
    let backend: Arc<dyn DynForgeBackend> = Arc::new(backend);
    let registry = SchemaRegistry::new();

    // Create schema: @access(read: ["viewer", "editor"], write: ["editor"], delete: ["admin"])
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
            read: vec!["viewer".into(), "editor".into()],
            write: vec!["editor".into()],
            delete: vec!["admin".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();
    register_schema(&schema, &backend, &registry).await;

    // --- Scenario A: "editor" can read and write ---
    println!("  Scenario A: editor role can read and write");
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["editor".into()]))),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Article/entities",
        Some(serde_json::json!({"fields": {"title": "My Article", "body": "Content"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    println!("    CREATE: {} (id={})", status, json["id"]);

    let (status, json) = json_request(&app, Method::GET, "/schemas/Article/entities", None).await;
    assert_eq!(status, StatusCode::OK);
    println!("    LIST:   {} ({} entities)", status, json["count"]);

    // --- Scenario B: "viewer" can read but NOT write ---
    println!("  Scenario B: viewer role can read but NOT write");
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["viewer".into()]))),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, _) = json_request(&app, Method::GET, "/schemas/Article/entities", None).await;
    assert_eq!(status, StatusCode::OK);
    println!("    LIST:   {} (read permitted)", status);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Article/entities",
        Some(serde_json::json!({"fields": {"title": "Blocked", "body": "Nope"}})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    println!("    CREATE: {} (write denied: {})", status, json["error"]);

    // --- Scenario C: "viewer" cannot delete ---
    println!("  Scenario C: viewer cannot delete");
    let (status, json) = json_request(
        &app,
        Method::DELETE,
        "/schemas/Article/entities/entity_fake_id",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    println!("    DELETE: {} (delete denied: {})", status, json["error"]);

    println!("  PASSED\n");
}

// ===========================================================================
// TEST 3: Field-level @field_access filtering (Phase 4)
// ===========================================================================

#[tokio::test]
async fn demo_field_access_filtering() {
    println!("\n=== DEMO: Field-Level @field_access Filtering ===");

    let backend = SurrealBackend::connect_memory("test", "demo_field")
        .await
        .unwrap();
    let backend: Arc<dyn DynForgeBackend> = Arc::new(backend);
    let registry = SchemaRegistry::new();

    // Schema: salary field only visible/writable by "hr" role
    // @access with empty lists = all authenticated users permitted (testing field-level, not schema-level)
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Employee").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("department").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_annotations(
                FieldName::new("salary").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::FieldAccess {
                    read: vec!["hr".into()],
                    write: vec!["hr".into()],
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
    register_schema(&schema, &backend, &registry).await;

    // --- HR user creates employee with salary ---
    println!("  HR user creates employee with salary field");
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["hr".into()]))),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Employee/entities",
        Some(serde_json::json!({"fields": {"name": "Alice", "department": "Engineering", "salary": "150000"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entity_id = json["id"].as_str().unwrap().to_string();
    println!(
        "    HR sees salary: {} (name={}, salary={})",
        status, json["fields"]["name"], json["fields"]["salary"]
    );
    assert_eq!(json["fields"]["salary"], "150000");

    // --- Regular member reads same employee ---
    println!("  Regular member reads same employee");
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["member".into()]))),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let path = format!("/schemas/Employee/entities/{entity_id}");
    let (status, json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    let has_salary = json["fields"].get("salary").is_some()
        && json["fields"]["salary"] != serde_json::Value::Null;
    println!(
        "    Member sees name={}, salary visible={}",
        json["fields"]["name"], has_salary
    );
    assert!(!has_salary, "salary should be filtered from member view");

    println!("  PASSED\n");
}

// ===========================================================================
// TEST 4: Record-level @owner enforcement (Phase 6)
// ===========================================================================

#[tokio::test]
async fn demo_record_ownership() {
    println!("\n=== DEMO: Record-Level @owner Enforcement ===");

    let backend = SurrealBackend::connect_memory("test", "demo_owner")
        .await
        .unwrap();
    let backend: Arc<dyn DynForgeBackend> = Arc::new(backend);
    let registry = SchemaRegistry::new();

    // Schema with @owner on owner_id field
    // @access with empty lists = all authenticated users permitted (testing ownership, not schema-level)
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Note").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("content").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_annotations(
                FieldName::new("owner_id").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::Owner],
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
    register_schema(&schema, &backend, &registry).await;

    let alice_id = EntityId::new();
    let bob_id = EntityId::new();

    // --- Alice creates a note ---
    println!("  Alice creates a note");
    let alice_provider = Arc::new(ConfigurableAuthProvider {
        user_id: alice_id.clone(),
        roles: vec!["member".into()],
        tenant_chain: vec![],
    });
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(alice_provider),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Note/entities",
        Some(serde_json::json!({
            "fields": {
                "content": "Alice's private note",
                "owner_id": alice_id.to_string()
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let note_id = json["id"].as_str().unwrap().to_string();
    println!("    Created note: {} (owner={})", note_id, alice_id);

    // --- Bob tries to update Alice's note ---
    println!("  Bob tries to update Alice's note");
    let bob_provider = Arc::new(ConfigurableAuthProvider {
        user_id: bob_id.clone(),
        roles: vec!["member".into()],
        tenant_chain: vec![],
    });
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(bob_provider),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let path = format!("/schemas/Note/entities/{note_id}");
    let (status, json) = json_request(
        &app,
        Method::PUT,
        &path,
        Some(serde_json::json!({"fields": {"content": "Bob was here"}})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    println!("    Bob UPDATE: {} ({})", status, json["error"]);

    // --- Bob tries to delete Alice's note ---
    println!("  Bob tries to delete Alice's note");
    let (status, json) = json_request(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    println!("    Bob DELETE: {} ({})", status, json["error"]);

    // --- Admin can modify anyone's note ---
    println!("  Admin overrides ownership check");
    let admin_provider = Arc::new(ConfigurableAuthProvider {
        user_id: bob_id.clone(),
        roles: vec!["admin".into()],
        tenant_chain: vec![],
    });
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(admin_provider),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(
        &app,
        Method::PUT,
        &path,
        Some(serde_json::json!({"fields": {"content": "Admin override", "owner_id": alice_id.to_string()}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    println!(
        "    Admin UPDATE: {} (content={})",
        status, json["fields"]["content"]
    );

    println!("  PASSED\n");
}

// ===========================================================================
// TEST 5: Multi-tenancy isolation (Phase 5)
// ===========================================================================

#[tokio::test]
async fn demo_multi_tenancy_isolation() {
    println!("\n=== DEMO: Multi-Tenancy Isolation ===");

    let surreal = SurrealBackend::connect_memory("test", "demo_tenant")
        .await
        .unwrap();
    let db_client = surreal.client().clone();
    let backend: Arc<dyn DynForgeBackend> = Arc::new(surreal);
    let registry = SchemaRegistry::new();

    // Create Organization schema with @tenant(root)
    // @access with empty lists = all authenticated users permitted (testing tenancy, not schema-level)
    let org_schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Organization").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![
            Annotation::Tenant(TenantKind::Root),
            Annotation::Access {
                read: vec![],
                write: vec![],
                delete: vec![],
                cross_tenant_read: vec![],
            },
        ],
    )
    .unwrap();
    register_schema(&org_schema, &backend, &registry).await;

    // Create Project schema (regular, will be tenant-scoped)
    // @access with empty lists = all authenticated users permitted (testing tenancy, not schema-level)
    let project_schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Project").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("title").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Access {
            read: vec![],
            write: vec![],
            delete: vec![],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();
    register_schema(&project_schema, &backend, &registry).await;

    // Define _tenant field on tenant-scoped tables (SCHEMAFULL requires explicit field definition)
    db_client
        .query("DEFINE FIELD _tenant ON Project TYPE option<string>;")
        .await
        .expect("define _tenant field");

    // Build tenant config from schemas
    let all_schemas = registry.list().await;
    let tenant_config = TenantConfig::from_schemas(&all_schemas).expect("valid tenant config");
    assert!(tenant_config.is_enabled());
    println!(
        "  Tenant config: root={:?}, levels={}",
        tenant_config.root_schema.as_ref().map(|s| s.as_str()),
        tenant_config.hierarchy.len()
    );

    let org_a_id = EntityId::new();
    let org_b_id = EntityId::new();

    // --- Tenant A creates a project ---
    println!("  Tenant A creates a project");
    let tenant_a_provider = Arc::new(ConfigurableAuthProvider {
        user_id: EntityId::new(),
        roles: vec!["member".into()],
        tenant_chain: vec![TenantRef {
            schema: SchemaName::new("Organization").unwrap(),
            entity_id: org_a_id.clone(),
        }],
    });
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(tenant_a_provider),
        tenant_config: Some(tenant_config.clone()),
        record_access_policy: None,
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Project/entities",
        Some(serde_json::json!({"fields": {"title": "Tenant A Project"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    println!(
        "    Created: {} (title={})",
        json["id"], json["fields"]["title"]
    );

    // Verify tenant A sees its project
    let (status, json) = json_request(&app, Method::GET, "/schemas/Project/entities", None).await;
    assert_eq!(status, StatusCode::OK);
    let tenant_a_count = json["count"].as_u64().unwrap();
    println!("    Tenant A lists: {} projects", tenant_a_count);
    assert_eq!(tenant_a_count, 1);

    // --- Tenant B creates a project ---
    println!("  Tenant B creates a project");
    let tenant_b_provider = Arc::new(ConfigurableAuthProvider {
        user_id: EntityId::new(),
        roles: vec!["member".into()],
        tenant_chain: vec![TenantRef {
            schema: SchemaName::new("Organization").unwrap(),
            entity_id: org_b_id.clone(),
        }],
    });
    let state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(tenant_b_provider),
        tenant_config: Some(tenant_config.clone()),
        record_access_policy: None,
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(state);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Project/entities",
        Some(serde_json::json!({"fields": {"title": "Tenant B Project"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    println!(
        "    Created: {} (title={})",
        json["id"], json["fields"]["title"]
    );

    // Tenant B should only see its OWN project (not Tenant A's)
    let (status, json) = json_request(&app, Method::GET, "/schemas/Project/entities", None).await;
    assert_eq!(status, StatusCode::OK);
    let tenant_b_count = json["count"].as_u64().unwrap();
    println!("    Tenant B lists: {} projects", tenant_b_count);
    assert_eq!(
        tenant_b_count, 1,
        "Tenant B should only see its own project"
    );

    // Verify it's actually Tenant B's project
    let tenant_b_title = json["entities"][0]["fields"]["title"].as_str().unwrap();
    assert_eq!(tenant_b_title, "Tenant B Project");
    println!("    Tenant B sees: \"{}\" (correct!)", tenant_b_title);

    println!("  PASSED\n");
}

// ===========================================================================
// TEST 6: DSL round-trip with all annotations (Phase 1)
// ===========================================================================

#[tokio::test]
async fn demo_dsl_roundtrip_all_annotations() {
    println!("\n=== DEMO: DSL Round-Trip with All Annotations ===");

    let dsl = r#"
@system @display("email") @access(read: ["admin"], write: ["admin"], delete: ["admin"])
schema SecureUser {
    email:        text(max: 512) required indexed
    display_name: text(max: 255) required
    salary:       float @field_access(read: ["hr"], write: ["hr"])
    owner_ref:    text @owner
    active:       boolean default(true)
}

@tenant(root)
schema Organization {
    name: text(max: 255) required
}

@tenant(parent: "Organization")
schema Department {
    name:   text(max: 255) required
    org:    -> Organization
}
"#;

    // Parse
    let schemas = schema_forge_dsl::parse(dsl).expect("parse should succeed");
    assert_eq!(schemas.len(), 3);
    println!("  Parsed {} schemas from DSL", schemas.len());

    // Print
    let printed = schema_forge_dsl::print_all(&schemas);
    println!(
        "  Printed DSL:\n{}",
        printed
            .trim()
            .lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Re-parse
    let reparsed = schema_forge_dsl::parse(&printed).expect("re-parse should succeed");
    assert_eq!(reparsed.len(), 3);

    // Compare
    for (orig, reparsed) in schemas.iter().zip(reparsed.iter()) {
        assert_eq!(orig.name, reparsed.name, "Schema name mismatch");
        assert_eq!(
            orig.fields.len(),
            reparsed.fields.len(),
            "Field count mismatch for {}",
            orig.name.as_str()
        );
        assert_eq!(
            orig.annotations.len(),
            reparsed.annotations.len(),
            "Annotation count mismatch for {}",
            orig.name.as_str()
        );

        // Compare field annotations
        for (of, rf) in orig.fields.iter().zip(reparsed.fields.iter()) {
            assert_eq!(of.name, rf.name);
            assert_eq!(
                of.annotations.len(),
                rf.annotations.len(),
                "Field annotation count mismatch for {}.{}",
                orig.name.as_str(),
                of.name.as_str()
            );
        }
    }
    println!(
        "  Round-trip comparison: all {} schemas match",
        schemas.len()
    );

    // Verify annotations parsed correctly
    let secure_user = &schemas[0];
    assert!(secure_user.is_system());
    assert!(secure_user.has_access_restrictions());
    println!(
        "  SecureUser: @system={}, @access={}, fields={}",
        secure_user.is_system(),
        secure_user.has_access_restrictions(),
        secure_user.fields.len()
    );

    let salary_field = secure_user.field("salary").unwrap();
    assert!(
        !salary_field.annotations.is_empty(),
        "salary should have field annotations"
    );
    println!(
        "  salary field: {} annotation(s)",
        salary_field.annotations.len()
    );

    let owner_field = secure_user.field("owner_ref").unwrap();
    assert!(owner_field.has_owner(), "owner_ref should have @owner");
    println!("  owner_ref: has_owner={}", owner_field.has_owner());

    println!("  PASSED\n");
}

// ===========================================================================
// TEST 7: Cedar policies from @access annotations (Phase 4)
// ===========================================================================

#[tokio::test]
async fn demo_cedar_policies_from_annotations() {
    println!("\n=== DEMO: Cedar Policy Generation from @access ===");

    // Schema with @access annotation
    let schema_with_access = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Invoice").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("amount").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Access {
            read: vec!["accountant".into(), "manager".into()],
            write: vec!["accountant".into()],
            delete: vec!["manager".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();

    let policies = schema_forge_acton::cedar::generate_cedar_policies(&schema_with_access);
    println!(
        "  Generated {} Cedar policies for Invoice schema:",
        policies.len()
    );
    for policy in &policies {
        println!("    - {}", policy.description);
    }
    assert!(!policies.is_empty());

    // Schema without @access — should get default policies
    let schema_no_access = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Contact").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap();

    let default_policies = schema_forge_acton::cedar::generate_cedar_policies(&schema_no_access);
    println!(
        "  Generated {} default Cedar policies for Contact schema:",
        default_policies.len()
    );
    for policy in &default_policies {
        println!("    - {}", policy.description);
    }
    assert_eq!(default_policies.len(), 4);

    println!("  PASSED\n");
}

// ===========================================================================
// TEST 8: Combined auth + field_access + owner (all layers)
// ===========================================================================

#[tokio::test]
async fn demo_all_auth_layers_combined() {
    println!("\n=== DEMO: All Auth Layers Combined ===");

    let backend = SurrealBackend::connect_memory("test", "demo_combined")
        .await
        .unwrap();
    let backend: Arc<dyn DynForgeBackend> = Arc::new(backend);
    let registry = SchemaRegistry::new();

    // Schema with EVERYTHING: @access, @field_access, @owner
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Document").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_annotations(
                FieldName::new("confidential_notes").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::FieldAccess {
                    read: vec!["manager".into()],
                    write: vec!["manager".into()],
                }],
            ),
            FieldDefinition::with_annotations(
                FieldName::new("author_id").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::Owner],
            ),
        ],
        vec![Annotation::Access {
            read: vec!["employee".into(), "manager".into()],
            write: vec!["employee".into(), "manager".into()],
            delete: vec!["manager".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();
    register_schema(&schema, &backend, &registry).await;

    let author_id = EntityId::new();

    // --- Step 1: Author (employee) creates document ---
    // @access: employee in write list → allowed
    // @field_access: employee not in manager → confidential_notes stripped from response
    println!("  Step 1: employee creates document");
    let employee_state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(ConfigurableAuthProvider {
            user_id: author_id.clone(),
            roles: vec!["employee".into()],
            tenant_chain: vec![],
        })),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(employee_state);

    let (status, json) = json_request(
        &app,
        Method::POST,
        "/schemas/Document/entities",
        Some(serde_json::json!({
            "fields": {
                "title": "Q4 Report",
                "confidential_notes": "Layoffs planned",
                "author_id": author_id.to_string()
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let doc_id = json["id"].as_str().unwrap().to_string();
    println!("    Created doc: {}", doc_id);
    // Employee can't read confidential_notes (@field_access restricts to manager role)
    let has_notes = json["fields"].get("confidential_notes").is_some()
        && json["fields"]["confidential_notes"] != serde_json::Value::Null;
    println!(
        "    Employee sees confidential_notes: {} (should be false)",
        has_notes
    );
    assert!(
        !has_notes,
        "Layer 3 (@field_access): employee should not see confidential_notes"
    );

    // --- Step 2: Non-owner manager is blocked by @owner ---
    // @access: manager in read list → allowed
    // @owner: manager is NOT the author → 403
    println!("  Step 2: non-owner manager blocked by @owner");
    let manager_state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(ConfigurableAuthProvider {
            user_id: EntityId::new(), // different user
            roles: vec!["manager".into()],
            tenant_chain: vec![],
        })),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(manager_state);

    let path = format!("/schemas/Document/entities/{doc_id}");
    let (status, _json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    println!(
        "    Manager GET: {} — Layer 2 (@owner) blocks non-owner",
        status
    );

    // --- Step 3: Admin bypasses ALL layers (schema, record, field) ---
    // @access: admin bypasses
    // @owner: admin bypasses
    // @field_access: admin bypasses
    println!("  Step 3: admin bypasses all layers");
    let admin_state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(ConfigurableAuthProvider {
            user_id: EntityId::new(), // not the author
            roles: vec!["admin".into()],
            tenant_chain: vec![],
        })),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(admin_state);

    let (status, json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    let admin_sees_notes = json["fields"].get("confidential_notes").is_some()
        && json["fields"]["confidential_notes"] != serde_json::Value::Null;
    println!(
        "    Admin GET: {} — sees confidential_notes: {}",
        status, admin_sees_notes
    );

    // --- Step 4: Guest blocked at schema level (@access) ---
    // @access: guest NOT in read list → 403 (never reaches @owner check)
    println!("  Step 4: guest blocked by @access (schema level)");
    let guest_state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(ConfigurableAuthProvider {
            user_id: EntityId::new(),
            roles: vec!["guest".into()],
            tenant_chain: vec![],
        })),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(guest_state);

    let (status, _json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    println!(
        "    Guest GET: {} — Layer 1 (@access) blocks non-listed role",
        status
    );

    // --- Step 5: Author (owner) can read but field_access filters notes ---
    println!("  Step 5: author reads own doc, but confidential_notes filtered");
    let author_state = ForgeState {
        registry: registry.clone(),
        backend: backend.clone(),
        auth_provider: Some(Arc::new(ConfigurableAuthProvider {
            user_id: author_id.clone(),
            roles: vec!["employee".into()],
            tenant_chain: vec![],
        })),
        tenant_config: None,
        record_access_policy: Some(Arc::new(OwnershipBasedPolicy)),
        #[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
        theme: test_theme(),
        #[cfg(feature = "graphql")]
        graphql_schema: test_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
        surreal_client: None,
        #[cfg(feature = "cloud-ui")]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::cloud::overrides::TemplateEngine::new(None),
        ),
    };
    let app = test_app_with_state(author_state);

    let (status, json) = json_request(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    let author_sees_title = json["fields"]["title"].as_str() == Some("Q4 Report");
    let author_sees_notes = json["fields"].get("confidential_notes").is_some()
        && json["fields"]["confidential_notes"] != serde_json::Value::Null;
    println!(
        "    Author GET: {} — title={}, confidential_notes={}",
        status, author_sees_title, author_sees_notes
    );
    assert!(author_sees_title, "owner should see title");
    assert!(
        !author_sees_notes,
        "Layer 3 (@field_access): employee owner should not see confidential_notes"
    );

    println!("  PASSED\n");
}
