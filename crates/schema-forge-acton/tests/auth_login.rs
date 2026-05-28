//! Integration tests for the JSON login endpoint (`POST /auth/login`).
//!
//! These tests mount **only** the auth sub-router (`auth_routes()`) onto a
//! synthetic axum router with the two Extensions the handler depends on:
//! a real in-memory SurrealBackend (playing the `DynAuthStore` role) and a
//! freshly minted PasetoGenerator backed by a 32-byte symmetric key.
//!
//! Scope cut: end-to-end verification that the minted token actually passes
//! the acton-service token middleware on a downstream protected route is
//! covered by the shell-side smoke test (see the task report) because
//! mounting the real middleware here requires the full ServiceBuilder
//! pipeline, which is the wrong level of fidelity for this file.

#![cfg(feature = "surrealdb")]

use std::sync::Arc;

use acton_service::auth::config::{PasetoGenerationConfig, TokenGenerationConfig};
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use schema_forge_acton::routes::auth_routes;
use schema_forge_acton::state::DynAuthStore;
use schema_forge_backend::{AuthStore, EntityAuthStore};
use schema_forge_core::types::SchemaDefinition;
use schema_forge_surrealdb::SurrealBackend;
use tempfile::NamedTempFile;
use tower::ServiceExt;

/// Seed an in-memory SurrealBackend with one known-good admin user.
///
/// Uses the production [`EntityAuthStore`] path so the test exercises
/// the real identity surface — `User` entity table, `password_hash`
/// behind `@hidden`. The schema is migrated to the in-memory backend
/// so entity create/get queries work.
async fn seeded_auth_store() -> Arc<dyn DynAuthStore> {
    use schema_forge_backend::traits::SchemaBackend;
    use schema_forge_core::migration::DiffEngine;

    let backend = SurrealBackend::connect_memory("test", "auth_login_test")
        .await
        .expect("connect in-memory surreal");

    // Apply the system User schema and register its metadata so the
    // SchemaId → table mapping exists.
    let user_schema = parse_user_schema();
    let plan = DiffEngine::create_new(&user_schema);
    backend
        .apply_migration(&user_schema.name, &plan.steps)
        .await
        .expect("apply User migration");
    backend
        .store_schema_metadata(&user_schema)
        .await
        .expect("store User schema metadata");

    let backend = Arc::new(backend);
    let entity_store: Arc<dyn schema_forge_backend::DynEntityStore> = backend.clone();

    let resolver: schema_forge_backend::entity_auth_store::RoleRankResolver =
        Arc::new(|_role: &str| None);
    let store = EntityAuthStore::new(entity_store, user_schema, resolver);
    AuthStore::create_user(&store, "admin", "dev", &["admin".to_string()], "Administrator")
        .await
        .expect("seed admin user");
    Arc::new(store)
}

/// Parse the system USER_SCHEMA DSL into a `SchemaDefinition` for tests.
fn parse_user_schema() -> SchemaDefinition {
    let mut schemas = schema_forge_dsl::parse(schema_forge_core::system_schemas::USER_SCHEMA)
        .expect("USER_SCHEMA must parse");
    schemas.pop().expect("USER_SCHEMA must yield one schema")
}

/// Write a random 32-byte V4.local key to a NamedTempFile and build a
/// matching `PasetoGenerator`. The tempfile is returned so the caller keeps
/// it alive for the duration of the test.
fn build_test_generator() -> (Arc<PasetoGenerator>, NamedTempFile) {
    let tmp = NamedTempFile::new().expect("tempfile");
    let key_bytes: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ 0xA5);
    std::fs::write(tmp.path(), key_bytes).expect("write key");

    let paseto_gen = PasetoGenerationConfig {
        version: "v4".to_string(),
        purpose: "local".to_string(),
        key_path: tmp.path().to_path_buf(),
        issuer: Some("schemaforge-test".to_string()),
        audience: None,
    };
    let token_gen = TokenGenerationConfig {
        access_token_lifetime_secs: 3600,
        issuer: Some("schemaforge-test".to_string()),
        audience: None,
        include_jti: true,
    };
    let generator = PasetoGenerator::new(&paseto_gen, &token_gen).expect("build generator");
    (Arc::new(generator), tmp)
}

/// Build a router that mounts only `/auth/login` with the Extensions the
/// login handler now depends on. The auth store is returned so tests can
/// read back the User row to verify side-effects (e.g. `last_login`).
///
/// `tenant_config` is `None` (tenancy disabled) — the basic suite covers
/// the historical single-tenant path. Tenancy-enabled tests use
/// [`login_app_with_tenancy`] instead.
async fn login_app() -> (Router, NamedTempFile, Arc<dyn DynAuthStore>) {
    let (router, key_tmp, store) = login_app_with(seeded_auth_store().await, None).await;
    (router, key_tmp, store)
}

/// Variant of [`login_app`] that lets the caller supply both a pre-seeded
/// auth store and an optional `TenantConfig`. Used by the multi-tenant
/// tests to exercise the zero-membership refusal and tenant_chain
/// projection paths.
async fn login_app_with(
    auth_store: Arc<dyn DynAuthStore>,
    tenant_config: Option<schema_forge_backend::tenant::TenantConfig>,
) -> (Router, NamedTempFile, Arc<dyn DynAuthStore>) {
    let (generator, key_tmp) = build_test_generator();
    let principal_claims =
        Arc::new(schema_forge_acton::authz::PrincipalClaimMappings::default());
    let tenant_layer: Arc<Option<schema_forge_backend::tenant::TenantConfig>> =
        Arc::new(tenant_config);
    let router = auth_routes()
        .layer(Extension(auth_store.clone()))
        .layer(Extension(generator))
        .layer(Extension(principal_claims))
        .layer(Extension(tenant_layer))
        .with_state(acton_service::state::AppState::<
            schema_forge_acton::SchemaForgeConfig,
        >::default());
    (router, key_tmp, auth_store)
}

async fn post_login(app: Router, body: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("response body is JSON");
    (status, body)
}

#[tokio::test]
async fn login_with_correct_credentials_returns_token() {
    let (app, _key, _store) = login_app().await;
    let (status, body) = post_login(app, r#"{"username":"admin","password":"dev"}"#).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().expect("token field is a string");
    assert!(
        token.starts_with("v4.local."),
        "expected v4.local.* token, got {token}"
    );
    let expires_at = body["expires_at"]
        .as_str()
        .expect("expires_at field is a string");
    let parsed =
        chrono::DateTime::parse_from_rfc3339(expires_at).expect("expires_at parses as RFC3339");
    assert!(
        parsed > chrono::Utc::now(),
        "expires_at {expires_at} should be in the future"
    );
}

#[tokio::test]
async fn login_with_wrong_password_returns_401_envelope() {
    let (app, _key, _store) = login_app().await;
    let (status, body) = post_login(app, r#"{"username":"admin","password":"wrong"}"#).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid credentials");
    assert_eq!(body["code"], "UNAUTHORIZED");
    assert_eq!(body["status"], 401);
}

#[tokio::test]
async fn login_with_unknown_user_returns_401_envelope() {
    let (app, _key, _store) = login_app().await;
    let (status, body) = post_login(app, r#"{"username":"ghost","password":"dev"}"#).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid credentials");
    assert_eq!(body["code"], "UNAUTHORIZED");
    assert_eq!(body["status"], 401);
}

/// Regression for issue #59: a successful login must stamp `last_login` on
/// the User row. Prior to the fix the field was declared by the schema but
/// no code ever wrote it, so admins could not answer "who logged in
/// recently?" from `entity list User`.
#[tokio::test]
async fn login_success_stamps_last_login_on_user_row() {
    use schema_forge_core::types::DynamicValue;

    let (app, _key, store) = login_app().await;

    let before = chrono::Utc::now();
    let (status, _body) = post_login(app, r#"{"username":"admin","password":"dev"}"#).await;
    assert_eq!(status, StatusCode::OK);

    let entity = store
        .get_user_entity("admin")
        .await
        .expect("get_user_entity ok")
        .expect("admin row must exist");
    let last_login = match entity.field("last_login") {
        Some(DynamicValue::DateTime(dt)) => *dt,
        other => panic!("expected DynamicValue::DateTime on last_login, got {other:?}"),
    };
    assert!(
        last_login >= before,
        "last_login {last_login} must be at-or-after the request start {before}"
    );
    assert!(
        last_login <= chrono::Utc::now(),
        "last_login {last_login} must not be in the future"
    );
}

// ---------------------------------------------------------------------------
// Tenancy / TenantMembership tests (issue #67)
// ---------------------------------------------------------------------------

/// Seed an in-memory backend with both the User and TenantMembership system
/// schemas migrated, and one user "alice" with `n_memberships` rows pointing
/// at distinct Organization IDs.
async fn seeded_auth_store_with_memberships(
    n_memberships: usize,
    roles: &[&str],
) -> (Arc<dyn DynAuthStore>, schema_forge_backend::tenant::TenantConfig) {
    use schema_forge_backend::traits::SchemaBackend;
    use schema_forge_backend::{DynEntityStore, Entity};
    use schema_forge_core::migration::DiffEngine;
    use schema_forge_core::types::DynamicValue;

    let backend = SurrealBackend::connect_memory("test", "auth_login_tenancy_test")
        .await
        .expect("connect in-memory surreal");

    // Migrate User schema.
    let user_schema = parse_user_schema();
    let plan = DiffEngine::create_new(&user_schema);
    backend
        .apply_migration(&user_schema.name, &plan.steps)
        .await
        .expect("apply User migration");
    backend
        .store_schema_metadata(&user_schema)
        .await
        .expect("store User schema metadata");

    // Migrate TenantMembership schema.
    let mut parsed_tm = schema_forge_dsl::parse(
        schema_forge_core::system_schemas::TENANT_MEMBERSHIP_SCHEMA,
    )
    .expect("TENANT_MEMBERSHIP_SCHEMA parses");
    let tm_schema = parsed_tm.pop().expect("one TenantMembership schema");
    let tm_plan = DiffEngine::create_new(&tm_schema);
    backend
        .apply_migration(&tm_schema.name, &tm_plan.steps)
        .await
        .expect("apply TenantMembership migration");
    backend
        .store_schema_metadata(&tm_schema)
        .await
        .expect("store TenantMembership schema metadata");

    // Build a minimal Organization schema marked @tenant(root) so the
    // TenantConfig we hand to the login handler reports tenancy enabled.
    use schema_forge_core::types::{
        Annotation, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaId,
        SchemaName as CoreSchemaName, TenantKind, TextConstraints,
    };
    let org_schema = SchemaDefinition::new(
        SchemaId::new(),
        CoreSchemaName::new("Organization").unwrap(),
        vec![FieldDefinition::with_annotations(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
            vec![],
        )],
        vec![Annotation::Tenant(TenantKind::Root)],
    )
    .unwrap();

    let tenant_config = schema_forge_backend::tenant::TenantConfig::from_schemas(
        std::slice::from_ref(&org_schema),
    )
    .unwrap();
    assert!(tenant_config.is_enabled());

    let backend = Arc::new(backend);
    let entity_store: Arc<dyn DynEntityStore> = backend.clone();

    let resolver: schema_forge_backend::entity_auth_store::RoleRankResolver =
        Arc::new(|_role: &str| None);
    let store = EntityAuthStore::new(entity_store.clone(), user_schema, resolver)
        .with_tenant_membership_schema(tm_schema.clone());

    let role_strings: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
    AuthStore::create_user(&store, "alice", "dev", &role_strings, "Alice")
        .await
        .expect("seed alice user");

    // Fetch alice's EntityId so we can write TenantMembership refs.
    let alice = AuthStore::get_user_entity(&store, "alice")
        .await
        .unwrap()
        .unwrap();

    for i in 0..n_memberships {
        let mut fields: std::collections::BTreeMap<String, DynamicValue> =
            std::collections::BTreeMap::new();
        fields.insert("user".into(), DynamicValue::Ref(alice.id.clone()));
        fields.insert(
            "tenant_type".into(),
            DynamicValue::Text("Organization".to_string()),
        );
        fields.insert(
            "tenant_id".into(),
            DynamicValue::Text(format!("org-{}", (b'a' + i as u8) as char)),
        );
        let row = Entity::new(tm_schema.name.clone(), fields);
        // DynEntityStore::create returns a boxed future; call it via the
        // trait method directly so we don't need a concrete EntityStore impl.
        entity_store
            .create(&row)
            .await
            .expect("seed TenantMembership row");
    }

    (Arc::new(store), tenant_config)
}

/// Decode a minted PASETO token to inspect its `tenant_chain` custom claim.
///
/// The login handler uses a generator built by [`build_test_generator`]
/// against a deterministic 32-byte key; this function builds a matching
/// `PasetoAuth` validator over the same key file so we can round-trip
/// the token without standing up the full middleware stack.
fn decode_tenant_chain(
    token: &str,
    key_path: &std::path::Path,
) -> Vec<schema_forge_backend::TenantRef> {
    use acton_service::config::PasetoConfig;
    use acton_service::middleware::{PasetoAuth, TokenValidator};
    let cfg = PasetoConfig {
        version: "v4".into(),
        purpose: "local".into(),
        key_path: key_path.to_path_buf(),
        issuer: Some("schemaforge-test".into()),
        audience: None,
        public_paths: Vec::new(),
    };
    let auth = PasetoAuth::new(&cfg).expect("build PasetoAuth");
    let claims = auth.validate_token(token).expect("token validates");
    claims
        .custom_claim_as::<Vec<schema_forge_backend::TenantRef>>("tenant_chain")
        .unwrap_or_default()
}

#[tokio::test]
async fn login_emits_tenant_chain_for_single_membership_user() {
    let (store, tenant_config) =
        seeded_auth_store_with_memberships(1, &["member"]).await;
    let (app, key_tmp, _store) = login_app_with(store, Some(tenant_config)).await;
    let (status, body) = post_login(app, r#"{"username":"alice","password":"dev"}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().expect("token field is a string");
    let chain = decode_tenant_chain(token, key_tmp.path());
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].schema, "Organization");
    assert_eq!(chain[0].entity_id, "org-a");
}

#[tokio::test]
async fn login_emits_full_membership_set_for_multi_membership_user() {
    let (store, tenant_config) =
        seeded_auth_store_with_memberships(2, &["member"]).await;
    let (app, key_tmp, _store) = login_app_with(store, Some(tenant_config)).await;
    let (status, body) = post_login(app, r#"{"username":"alice","password":"dev"}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().expect("token field is a string");
    let chain = decode_tenant_chain(token, key_tmp.path());
    assert_eq!(chain.len(), 2);
    let ids: Vec<&str> = chain.iter().map(|t| t.entity_id.as_str()).collect();
    assert!(ids.contains(&"org-a"));
    assert!(ids.contains(&"org-b"));
}

#[tokio::test]
async fn login_refuses_zero_memberships_when_tenancy_enabled() {
    let (store, tenant_config) =
        seeded_auth_store_with_memberships(0, &["member"]).await;
    let (app, _key, _store) = login_app_with(store, Some(tenant_config)).await;
    let (status, body) = post_login(app, r#"{"username":"alice","password":"dev"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"], "no tenant assigned");
    assert_eq!(body["code"], "UNAUTHORIZED");
    assert_eq!(body["status"], 401);
}

#[tokio::test]
async fn login_allows_platform_admin_with_zero_memberships() {
    let (store, tenant_config) =
        seeded_auth_store_with_memberships(0, &["platform_admin"]).await;
    let (app, key_tmp, _store) = login_app_with(store, Some(tenant_config)).await;
    let (status, body) = post_login(app, r#"{"username":"alice","password":"dev"}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().expect("token field is a string");
    // No memberships => no tenant_chain claim on the minted token.
    let chain = decode_tenant_chain(token, key_tmp.path());
    assert!(chain.is_empty());
}

/// Failed credential validation must not stamp `last_login` — that field
/// records *successful* logins only.
#[tokio::test]
async fn login_failure_leaves_last_login_untouched() {
    let (app, _key, store) = login_app().await;

    let (status, _body) = post_login(app, r#"{"username":"admin","password":"wrong"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let entity = store
        .get_user_entity("admin")
        .await
        .expect("get_user_entity ok")
        .expect("admin row must exist");
    assert!(
        entity.field("last_login").is_none(),
        "last_login must remain unset after a failed login"
    );
}

// ---------------------------------------------------------------------------
// GET /auth/me (issue #70)
// ---------------------------------------------------------------------------

/// Build a `Claims` envelope the way the token middleware would inject one,
/// so `/auth/me` can be driven without standing up the full middleware stack.
fn claims_for(username: &str, roles: &[&str]) -> acton_service::middleware::Claims {
    use acton_service::auth::tokens::ClaimsBuilder;
    let mut b = ClaimsBuilder::new()
        .user(username)
        .username(username)
        .issuer("schemaforge");
    for r in roles {
        b = b.role(*r);
    }
    b.build().expect("build claims")
}

/// GET `/auth/me` with an optional injected `Claims` extension and an optional
/// `X-Active-Tenant` header.
async fn get_me(
    app: Router,
    claims: Option<acton_service::middleware::Claims>,
    active_tenant: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(Method::GET).uri("/auth/me");
    if let Some(h) = active_tenant {
        builder = builder.header("x-active-tenant", h);
    }
    let mut req = builder.body(Body::empty()).unwrap();
    if let Some(c) = claims {
        req.extensions_mut().insert(c);
    }
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body is JSON")
    };
    (status, body)
}

#[tokio::test]
async fn me_returns_principal_and_full_membership_set() {
    let (store, _cfg) = seeded_auth_store_with_memberships(2, &["member"]).await;
    let (app, _key, _store) = login_app_with(store, None).await;

    let (status, body) = get_me(app, Some(claims_for("alice", &["member"])), None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], "alice");
    assert!(
        body["user_id"].as_str().unwrap().starts_with("user_"),
        "user_id should be the User entity id, got {:?}",
        body["user_id"]
    );
    let chain = body["tenant_chain"].as_array().expect("tenant_chain array");
    assert_eq!(chain.len(), 2, "alice has two memberships");
    // Multiple memberships and no X-Active-Tenant header => no resolved active
    // tenant; the client must choose. (The header model shipped in #67.)
    assert!(body["active_tenant"].is_null());
    assert_eq!(body["active_tenant_header"], "x-active-tenant");
}

#[tokio::test]
async fn me_resolves_active_tenant_from_header() {
    let (store, _cfg) = seeded_auth_store_with_memberships(2, &["member"]).await;
    let (app, _key, _store) = login_app_with(store, None).await;

    let (status, body) = get_me(
        app,
        Some(claims_for("alice", &["member"])),
        Some("Organization:org-b"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active_tenant"]["tenant_type"], "Organization");
    assert_eq!(body["active_tenant"]["tenant_id"], "org-b");
}

#[tokio::test]
async fn me_with_non_member_active_tenant_header_resolves_null() {
    let (store, _cfg) = seeded_auth_store_with_memberships(2, &["member"]).await;
    let (app, _key, _store) = login_app_with(store, None).await;

    let (status, body) = get_me(
        app,
        Some(claims_for("alice", &["member"])),
        Some("Organization:org-not-mine"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body["active_tenant"].is_null(),
        "a header naming a non-member tenant must not resolve"
    );
}

#[tokio::test]
async fn me_without_claims_returns_401() {
    let (store, _cfg) = seeded_auth_store_with_memberships(1, &["member"]).await;
    let (app, _key, _store) = login_app_with(store, None).await;

    let (status, _body) = get_me(app, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
