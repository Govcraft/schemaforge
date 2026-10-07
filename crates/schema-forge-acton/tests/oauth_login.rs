#![cfg(feature = "oauth")]
//! The public redirect/exchange contract using a provider with no network.
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use acton_service::{
    auth::{
        config::{PasetoGenerationConfig, TokenGenerationConfig},
        oauth::{MemoryOAuthStateManager, OAuthProvider, OAuthTokens, OAuthUserInfo},
        tokens::paseto_generator::PasetoGenerator,
    },
    config::{Config, PasetoConfig},
    middleware::{PasetoAuth, TokenValidator},
    state::AppState,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
    Extension, Router,
};
use http_body_util::BodyExt;
use reqwest::Url;
use schema_forge_acton::{
    authz::PrincipalClaimMappings,
    config::SchemaForgeConfig,
    invite::{mint_invite_token, InviteTokenParams},
    oauth_config::{OAuthSettings, SignupPolicy},
    routes::{
        auth::auth_routes_with_password_login,
        oauth::{oauth_routes, OAuthLoginServices, OAuthRuntime},
    },
    state::DynAuthStore,
    tenancy_config::DefaultTenant,
};
use schema_forge_backend::{
    invite_store::{
        EntityInviteStore, InviteStatus, InviteStore, NewInvitation, FORGE_INVITATION_SCHEMA,
    },
    tenant::TenantConfig,
    EntityAuthStore, EntityStore, SchemaBackend,
};
use schema_forge_core::{
    migration::DiffEngine,
    system_schemas,
    types::{EntityId, SchemaName},
};
use schema_forge_surrealdb::SurrealBackend;
use serde_json::{json, Value};
use tempfile::NamedTempFile;
use tower::ServiceExt;

struct MockProvider {
    info: OAuthUserInfo,
}
#[async_trait::async_trait]
impl OAuthProvider for MockProvider {
    fn name(&self) -> &str {
        "github"
    }
    fn authorization_url(&self, state: &str, _: &[String]) -> String {
        let mut url = Url::parse("https://provider.example/authorize").unwrap();
        url.query_pairs_mut().append_pair("state", state);
        url.into()
    }
    async fn exchange_code(&self, code: &str) -> Result<OAuthTokens, acton_service::error::Error> {
        if code != "provider-code" {
            return Err(acton_service::error::Error::Unauthorized("bad code".into()));
        }
        Ok(OAuthTokens {
            access_token: "provider-access-token".into(),
            refresh_token: None,
            expires_in: None,
            token_type: "Bearer".into(),
            id_token: None,
        })
    }
    async fn get_user_info(&self, _: &str) -> Result<OAuthUserInfo, acton_service::error::Error> {
        Ok(self.info.clone())
    }
    async fn refresh_token(&self, _: &str) -> Result<OAuthTokens, acton_service::error::Error> {
        Err(acton_service::error::Error::Unauthorized("unused".into()))
    }
}

struct Options {
    settings: OAuthSettings,
    info: OAuthUserInfo,
    state_ttl: u64,
    code_ttl: u64,
    tenancy: bool,
    default_tenant: Option<DefaultTenant>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            settings: OAuthSettings {
                enabled: true,
                signup: SignupPolicy::Open,
                default_roles: vec!["member".into()],
                return_to_allowlist: vec!["https://console.example/app/".into()],
                password_login: true,
            },
            info: OAuthUserInfo {
                provider: "github".into(),
                provider_user_id: "123".into(),
                email: Some("alice@example.com".into()),
                email_verified: true,
                name: Some("Alice".into()),
                picture: None,
                raw: json!({}),
            },
            state_ttl: 600,
            code_ttl: 60,
            tenancy: false,
            default_tenant: None,
        }
    }
}
struct Fixture {
    app: Router,
    store: Arc<dyn DynAuthStore>,
    services: OAuthLoginServices,
    backend: Arc<SurrealBackend>,
    _key: NamedTempFile,
}
async fn fixture(options: Options) -> Fixture {
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("oauth_login", "oauth_login")
            .await
            .unwrap(),
    );
    let mut schemas = BTreeMap::new();
    for text in system_schemas::all_system_schemas()
        .into_iter()
        .chain([FORGE_INVITATION_SCHEMA])
    {
        let schema = schema_forge_dsl::parse(text).unwrap().remove(0);
        backend
            .apply_migration(&schema.name, &DiffEngine::create_new(&schema).steps)
            .await
            .unwrap();
        backend.store_schema_metadata(&schema).await.unwrap();
        schemas.insert(schema.name.to_string(), schema);
    }
    let user_schema = schemas["User"].clone();
    let store: Arc<dyn DynAuthStore> = Arc::new(
        EntityAuthStore::new(backend.clone(), user_schema.clone(), Arc::new(|_| Some(10)))
            .with_oauth_identity_schema(schemas["OAuthIdentity"].clone())
            .with_tenant_membership_schema(schemas["TenantMembership"].clone()),
    );
    let invites: Arc<dyn InviteStore> = Arc::new(EntityInviteStore::new(
        backend.clone(),
        schemas["ForgeInvitation"].clone(),
    ));
    let key = NamedTempFile::new().unwrap();
    std::fs::write(key.path(), [0xAC; 32]).unwrap();
    let generator = Arc::new(
        PasetoGenerator::new(
            &PasetoGenerationConfig {
                version: "v4".into(),
                purpose: "local".into(),
                key_path: key.path().into(),
                issuer: Some("oauth-test".into()),
                audience: None,
            },
            &TokenGenerationConfig {
                access_token_lifetime_secs: 3600,
                issuer: Some("oauth-test".into()),
                audience: None,
                include_jti: true,
            },
        )
        .unwrap(),
    );
    let validator = Arc::new(
        PasetoAuth::new(&PasetoConfig {
            version: "v4".into(),
            purpose: "local".into(),
            key_path: key.path().into(),
            issuer: Some("oauth-test".into()),
            audience: None,
            public_paths: Vec::new(),
        })
        .unwrap(),
    );
    // Exercise a real user_field projection in every successful callback.
    let mapping = serde_json::from_value(json!({"account_name": {"type": "string", "required": true, "source": {"user_field": "display_name"}}})).unwrap();
    let mut principal_claims = PrincipalClaimMappings::from_config(&mapping).unwrap();
    principal_claims
        .resolve_user_field_sources(&user_schema)
        .unwrap();
    let services = OAuthLoginServices {
        auth_store: store.clone(),
        generator,
        validator,
        invites,
        principal_claims: Arc::new(principal_claims),
        tenant_config: Arc::new(options.tenancy.then(|| TenantConfig {
            root_schema: Some(SchemaName::new("Organization").unwrap()),
            hierarchy: vec![],
        })),
    };
    let providers = BTreeMap::from([(
        "github".into(),
        Box::new(MockProvider { info: options.info }) as Box<dyn OAuthProvider>,
    )]);
    let runtime = Arc::new(
        OAuthRuntime::with_providers(
            options.settings.clone(),
            providers,
            Arc::new(MemoryOAuthStateManager::new(options.state_ttl)),
            Arc::new(MemoryOAuthStateManager::new(options.code_ttl)),
        )
        .unwrap()
        .with_default_tenant(options.default_tenant.clone()),
    );
    let mut config = Config::<SchemaForgeConfig>::default();
    config.custom.schema_forge.auth.oauth = options.settings.clone();
    let state = AppState::builder().config(config).build().await.unwrap();
    let auth = auth_routes_with_password_login(
        !options.settings.enabled || options.settings.password_login,
    );
    let auth = if options.settings.enabled {
        auth.merge(oauth_routes())
    } else {
        auth
    };
    let app = auth
        .layer(Extension(runtime))
        .layer(Extension(services.clone()))
        .layer(Extension(store.clone()))
        .layer(Extension(services.generator.clone()))
        .layer(Extension(services.principal_claims.clone()))
        .layer(Extension(services.tenant_config.clone()))
        .with_state(state);
    Fixture {
        app,
        store,
        services,
        backend,
        _key: key,
    }
}
async fn request(app: &Router, method: &str, uri: &str, value: Option<Value>) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(
                    value.map(|value| value.to_string()).unwrap_or_default(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn body(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
async fn start(app: &Router, invite: Option<&str>) -> String {
    let mut uri = Url::parse("http://forge/auth/oauth/github/start").unwrap();
    uri.query_pairs_mut().append_pair(
        "return_to",
        "https://console.example/app/welcome?view=account",
    );
    if let Some(invite) = invite {
        uri.query_pairs_mut().append_pair("invite_id", invite);
    }
    let response = request(
        app,
        "GET",
        &format!("{}?{}", uri.path(), uri.query().unwrap()),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let target = Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    target
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned()
}
async fn callback(app: &Router, state: &str) -> Response {
    request(
        app,
        "GET",
        &format!("/auth/oauth/github/callback?code=provider-code&state={state}"),
        None,
    )
    .await
}
fn login_code(response: Response) -> String {
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response.headers()["location"].to_str().unwrap();
    assert!(!location.contains("v4.local."));
    let target = Url::parse(location).unwrap();
    assert_eq!(
        target
            .query_pairs()
            .find(|(name, _)| name == "view")
            .unwrap()
            .1,
        "account"
    );
    target
        .query_pairs()
        .find(|(name, _)| name == "code")
        .unwrap()
        .1
        .into_owned()
}

#[tokio::test]
async fn callback_exchange_reuses_login_claims_and_identity_without_password() {
    let fixture = fixture(Options::default()).await;
    let state = start(&fixture.app, None).await;
    let code = login_code(callback(&fixture.app, &state).await);
    assert_eq!(
        callback(&fixture.app, &state).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let response = request(
        &fixture.app,
        "POST",
        "/auth/oauth/exchange",
        Some(json!({"code": code})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let result = body(response).await;
    assert_eq!(result["roles"], json!(["member"]));
    let claims = fixture
        .services
        .validator
        .validate_token(result["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(claims.sub, "user:alice@example.com");
    assert_eq!(claims.custom["account_name"], "Alice");
    let authenticated = fixture.app.clone().layer(Extension(claims.clone()));
    let me = body(request(&authenticated, "GET", "/auth/me", None).await).await;
    assert_eq!(
        me["identities"],
        json!([{"provider": "github", "subject": "123"}])
    );
    fixture
        .store
        .update_user("alice@example.com", &["updated".into()], "Current Alice")
        .await
        .unwrap();
    let refreshed = request(&authenticated, "POST", "/auth/refresh", None).await;
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed = body(refreshed).await;
    let refreshed_claims = fixture
        .services
        .validator
        .validate_token(refreshed["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(refreshed_claims.roles, ["updated"]);
    assert_eq!(refreshed_claims.custom["account_name"], "Current Alice");

    assert_eq!(
        fixture
            .store
            .list_identities("alice@example.com")
            .await
            .unwrap()
            .len(),
        1
    );
    let row = fixture
        .store
        .get_user_entity("alice@example.com")
        .await
        .unwrap()
        .unwrap();
    assert!(row.field("password_hash").is_none());
    assert!(row.field("last_login").is_some());
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/auth/login",
            Some(json!({"username": "alice@example.com", "password": "anything"}))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/auth/oauth/exchange",
            Some(json!({"code": code}))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let state = start(&fixture.app, None).await;
    login_code(callback(&fixture.app, &state).await);
    assert_eq!(fixture.store.count_users().await.unwrap(), 1);
}

#[tokio::test]
async fn callback_refuses_unverified_missing_and_malformed_email() {
    for email in [
        None,
        Some("alice@example.com"),
        Some("malformed"),
        Some("alice@ex ample.com"),
    ] {
        let mut options = Options::default();
        options.info.email = email.map(str::to_owned);
        options.info.email_verified = email != Some("alice@example.com");
        let fixture = fixture(options).await;
        let state = start(&fixture.app, None).await;
        assert_eq!(
            callback(&fixture.app, &state).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(fixture.store.count_users().await.unwrap(), 0);
    }
}

#[tokio::test]
async fn providers_are_public_and_unknown_providers_and_unsafe_returns_are_rejected() {
    let fixture = fixture(Options::default()).await;
    assert_eq!(
        body(request(&fixture.app, "GET", "/auth/oauth/providers", None).await).await,
        json!(["github"])
    );
    for path in [
        "/auth/oauth/unknown/start?return_to=https://console.example/app/",
        "/auth/oauth/unknown/callback?code=x&state=x",
    ] {
        assert_eq!(
            request(&fixture.app, "GET", path, None).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(
            &fixture.app,
            "GET",
            "/auth/oauth/github/start?return_to=https://console.example.evil/app/",
            None
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        callback(&fixture.app, "invalid").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn expired_state_and_exchange_codes_are_refused() {
    let fixture = fixture(Options {
        state_ttl: 0,
        ..Default::default()
    })
    .await;
    let state = start(&fixture.app, None).await;
    assert_eq!(
        callback(&fixture.app, &state).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let fixture = self::fixture(Options {
        code_ttl: 0,
        ..Default::default()
    })
    .await;
    let state = start(&fixture.app, None).await;
    let code = login_code(callback(&fixture.app, &state).await);
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/auth/oauth/exchange",
            Some(json!({"code": code}))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn invitation_only_requires_invite_and_existing_email_is_never_linked() {
    let mut options = Options::default();
    options.settings.signup = SignupPolicy::InviteOnly;
    let fixture = fixture(options).await;
    let state = start(&fixture.app, None).await;
    assert_eq!(
        callback(&fixture.app, &state).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(fixture.store.count_users().await.unwrap(), 0);
    fixture
        .store
        .create_user("alice@example.com", "secret", &["admin".into()], "Original")
        .await
        .unwrap();
    let state = start(&fixture.app, None).await;
    assert_eq!(
        callback(&fixture.app, &state).await.status(),
        StatusCode::CONFLICT
    );
    assert!(fixture
        .store
        .list_identities("alice@example.com")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn password_login_can_be_disabled_and_missing_membership_refuses_oauth() {
    let mut options = Options::default();
    options.settings.password_login = false;
    let fixture = fixture(options).await;
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/auth/login",
            Some(json!({"username": "alice", "password": "secret"}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let fixture = self::fixture(Options {
        tenancy: true,
        ..Default::default()
    })
    .await;
    let state = start(&fixture.app, None).await;
    assert_eq!(
        callback(&fixture.app, &state).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn open_signup_adds_default_membership_before_issuing_tenant_session() {
    let tenant_id = EntityId::new("organization");
    let fixture = fixture(Options {
        tenancy: true,
        default_tenant: Some(DefaultTenant {
            schema: SchemaName::new("Organization").unwrap(),
            id: tenant_id.clone(),
            role: "owner".into(),
        }),
        ..Default::default()
    })
    .await;
    let state = start(&fixture.app, None).await;
    let code = login_code(callback(&fixture.app, &state).await);
    let result = body(
        request(
            &fixture.app,
            "POST",
            "/auth/oauth/exchange",
            Some(json!({"code": code})),
        )
        .await,
    )
    .await;
    assert_eq!(result["roles"], json!(["member"]));
    let claims = fixture
        .services
        .validator
        .validate_token(result["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        claims.custom["tenant_chain"],
        json!([{"schema": "Organization", "entity_id": tenant_id.as_str()}])
    );
    assert_eq!(
        claims.custom["tenant_roles"],
        json!([{"tenant": {"schema": "Organization", "entity_id": tenant_id.as_str()}, "role": "owner"}])
    );
    let memberships = fixture
        .store
        .list_tenant_memberships("alice@example.com")
        .await
        .unwrap();
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].schema, "Organization");
    assert_eq!(memberships[0].entity_id, tenant_id.as_str());
    let schema = fixture
        .backend
        .load_schema_metadata(&SchemaName::new("TenantMembership").unwrap())
        .await
        .unwrap()
        .unwrap();
    let rows = fixture
        .backend
        .query(&schema_forge_core::query::Query::new(schema.id))
        .await
        .unwrap();
    assert_eq!(rows.entities.len(), 1);
    assert_eq!(
        rows.entities[0].fields["role"],
        schema_forge_core::types::DynamicValue::Text("owner".into())
    );
}

#[tokio::test]
async fn invitation_signed_claims_override_mutable_columns_and_consumption_is_last() {
    let mut options = Options::default();
    options.settings.signup = SignupPolicy::InviteOnly;
    options.tenancy = true;
    options.default_tenant = Some(DefaultTenant {
        schema: SchemaName::new("Organization").unwrap(),
        id: EntityId::new("organization"),
        role: "member".into(),
    });
    let fixture = fixture(options).await;
    let minted = mint_invite_token(
        &fixture.services.generator,
        &InviteTokenParams {
            email: "alice@example.com".into(),
            role: Some("invited".into()),
            tenant_type: Some("Organization".into()),
            tenant_id: Some("org_signed".into()),
        },
        Duration::from_secs(3600),
    )
    .unwrap();
    let invite = fixture
        .services
        .invites
        .create(NewInvitation {
            email: "tampered@example.com".into(),
            display_name: Some("Invited Alice".into()),
            tenant_type: Some("Other".into()),
            tenant_id: Some("tampered".into()),
            role: Some("platform_admin".into()),
            jti: minted.invite_id.clone(),
            token: minted.token,
            expires_at: minted.expires_at,
            invited_by: None,
        })
        .await
        .unwrap();
    let state = start(&fixture.app, Some(&invite.jti)).await;
    let code = login_code(callback(&fixture.app, &state).await);
    let result = body(
        request(
            &fixture.app,
            "POST",
            "/auth/oauth/exchange",
            Some(json!({"code": code})),
        )
        .await,
    )
    .await;
    assert_eq!(result["roles"], json!(["invited"]));
    let claims = fixture
        .services
        .validator
        .validate_token(result["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(claims.custom["account_name"], "Invited Alice");
    assert_eq!(
        claims.custom["tenant_chain"],
        json!([{"schema": "Organization", "entity_id": "org_signed"}])
    );
    assert_eq!(
        fixture
            .services
            .invites
            .find_by_jti(&invite.jti)
            .await
            .unwrap()
            .unwrap()
            .status,
        InviteStatus::Consumed
    );
    // Keep the backend in the fixture so future migration/projection checks can
    // inspect the actual storage, rather than a replacement mock auth store.
    let row = fixture
        .store
        .get_user_entity("alice@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .backend
            .get(&SchemaName::new("User").unwrap(), &row.id)
            .await
            .unwrap(),
        row
    );
}

#[tokio::test]
async fn account_disabled_between_callback_and_exchange_refuses_token() {
    let fixture = fixture(Options::default()).await;
    let state = start(&fixture.app, None).await;
    let code = login_code(callback(&fixture.app, &state).await);
    fixture
        .store
        .toggle_user_active("alice@example.com")
        .await
        .unwrap();
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/auth/oauth/exchange",
            Some(json!({"code": code}))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn disabled_oauth_routes_and_disabled_password_route_are_absent() {
    let mut options = Options::default();
    options.settings.enabled = false;
    let fixture = fixture(options).await;
    for path in [
        "/auth/oauth/providers",
        "/auth/oauth/github/start",
        "/auth/oauth/github/callback",
    ] {
        assert_eq!(
            request(&fixture.app, "GET", path, None).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/auth/oauth/exchange",
            Some(json!({}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let mut options = Options::default();
    options.settings.password_login = false;
    let fixture = self::fixture(options).await;
    assert_eq!(
        request(&fixture.app, "POST", "/auth/login", Some(json!({})))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}
