//! Invitation policy and delivery regressions for issue 208.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use acton_service::auth::config::TokenGenerationConfig;
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::config::Config;
use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::service_builder::ServiceBuilder;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use schema_forge_acton::authz::{
    PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks,
};
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_acton::email::{EmailDelivery, EmailError, EmailMessage, EmailSender};
use schema_forge_acton::messages::{InitForge, ReplyChannel};
use schema_forge_acton::routes::forge_routes;
use schema_forge_acton::state::{DynAuthStore, DynForgeBackend};
use schema_forge_acton::ForgeActor;
use schema_forge_backend::tenant::{TenantConfig, TenantLevel};
use schema_forge_backend::traits::SchemaBackend;
use schema_forge_backend::{
    BackendError, EntityAuthStore, ForgeInvitation, InviteStatus, InviteStore, NewInvitation,
};
use schema_forge_core::types::{EntityId, SchemaName};
use schema_forge_surrealdb::SurrealBackend;
use tokio::sync::oneshot;
use tower::ServiceExt;

#[derive(Default)]
struct MemoryInvites(Mutex<Vec<ForgeInvitation>>);
#[async_trait]
impl InviteStore for MemoryInvites {
    async fn create(&self, invite: NewInvitation) -> Result<ForgeInvitation, BackendError> {
        let stored = ForgeInvitation {
            id: EntityId::new("entity"),
            email: invite.email,
            display_name: invite.display_name,
            tenant_type: invite.tenant_type,
            tenant_id: invite.tenant_id,
            role: invite.role,
            jti: invite.jti,
            token: invite.token,
            status: InviteStatus::Pending,
            expires_at: Some(invite.expires_at),
            invited_by: invite.invited_by,
            consumed_at: None,
        };
        self.0.lock().unwrap().push(stored.clone());
        Ok(stored)
    }
    async fn find_by_jti(&self, jti: &str) -> Result<Option<ForgeInvitation>, BackendError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.jti == jti)
            .cloned())
    }
    async fn mark_consumed(&self, id: &EntityId, at: DateTime<Utc>) -> Result<(), BackendError> {
        for invite in self.0.lock().unwrap().iter_mut().filter(|i| &i.id == id) {
            invite.status = InviteStatus::Consumed;
            invite.consumed_at = Some(at);
        }
        Ok(())
    }
}
#[derive(Default)]
struct FailingEmail(Mutex<usize>);
#[async_trait]
impl EmailSender for FailingEmail {
    async fn send(&self, _message: EmailMessage) -> Result<(), EmailError> {
        *self.0.lock().unwrap() += 1;
        Err(EmailError::Transport(
            "private relay diagnostics secret-password".into(),
        ))
    }
    fn public_base_url(&self) -> Option<&str> {
        Some("https://app.example.gov")
    }
}

async fn app(
    delivery: EmailDelivery,
    action: &str,
) -> (Router, Arc<MemoryInvites>, Arc<FailingEmail>) {
    let backend = Arc::new(
        SurrealBackend::connect_memory("invites", "invites")
            .await
            .unwrap(),
    );
    let user_schema = schema_forge_dsl::parse(schema_forge_core::system_schemas::USER_SCHEMA)
        .unwrap()
        .remove(0);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&user_schema);
    backend
        .apply_migration(&user_schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&user_schema).await.unwrap();
    let ranks = RoleRanks::from_toml_str("[roles]\nowner = 20\nmember = 10\nadmin = 30").unwrap();
    let custom = tempfile::tempdir().unwrap();
    std::fs::write(
        custom.path().join("invite.cedar"),
        format!(
            r#"
permit(principal in Forge::Group::"owner", action == Action::"{action}User", resource is User)
when {{ resource has "_tenant" && principal in resource["_tenant"] }};
"#
        ),
    )
    .unwrap();
    let snapshot = PolicyStoreSnapshot::from_schemas(
        std::slice::from_ref(&user_schema),
        Some(custom.path()),
        ranks.clone(),
        PrincipalClaimMappings::default(),
    )
    .unwrap();
    let policies = Arc::new(PolicyStore::new(snapshot));
    let resolver = Arc::new(move |role: &str| ranks.get(role));
    let auth_store: Arc<dyn DynAuthStore> = Arc::new(EntityAuthStore::new(
        backend.clone(),
        user_schema.clone(),
        resolver,
    ));
    let mut config = Config::<SchemaForgeConfig>::default();
    config.custom.schema_forge.email.delivery = delivery;
    // Link mode deliberately works with SMTP disabled and no host/from.
    config.custom.schema_forge.email.public_base_url = Some("https://app.example.gov".into());
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_actor::<ForgeActor>()
        .build();
    let (tx, rx) = oneshot::channel();
    let backend_dyn: Arc<dyn DynForgeBackend> = backend;
    service
        .state()
        .actor::<ForgeActor>()
        .unwrap()
        .send(InitForge {
            registry: HashMap::from([("User".into(), user_schema)]),
            backend: backend_dyn,
            tenant_config: Some(TenantConfig {
                root_schema: Some(SchemaName::new("Organization").unwrap()),
                hierarchy: vec![TenantLevel {
                    schema: SchemaName::new("Organization").unwrap(),
                    parent: None,
                    parent_field: None,
                }],
            }),
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: schema_forge_acton::storage::StorageRegistry::default(),
            policy_store: Some(policies),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    let mut claims: Claims = serde_json::from_value(serde_json::json!({
        "sub": "owner@example.gov", "roles": ["owner"], "perms": [], "exp": 9999999999_u64
    }))
    .unwrap();
    claims.custom.insert(
        "tenant_chain".into(),
        serde_json::json!([{"schema": "Organization", "entity_id": "org_a"}]),
    );
    let store = Arc::new(MemoryInvites::default());
    let sender = Arc::new(FailingEmail::default());
    let invite_store: Arc<dyn InviteStore> = store.clone();
    let email_sender: Arc<dyn EmailSender> = sender.clone();
    let generator = Arc::new(PasetoGenerator::with_symmetric_key(
        [11; 32],
        TokenGenerationConfig::default(),
    ));
    let app = forge_routes()
        .merge(schema_forge_acton::routes::auth::auth_routes())
        .layer(Extension(auth_store))
        .layer(Extension(invite_store))
        .layer(Extension(email_sender))
        .layer(Extension(generator))
        .layer(Extension(claims))
        .with_state(service.state().clone());
    (app, store, sender)
}
async fn post(
    app: &Router,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn invite(role: &str, tenant: &str) -> serde_json::Value {
    serde_json::json!({"email":"new@example.gov", "role":role, "tenant_type":"Organization", "tenant_id":tenant})
}

#[tokio::test(flavor = "multi_thread")]
async fn link_invite_uses_scoped_invite_permission_without_smtp_or_create_permission() {
    let (app, store, sender) = app(EmailDelivery::Link, "Invite").await;
    let (status, body) = post(&app, "/auth/invites", invite("member", "org_a")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["delivery"], "link");
    assert_eq!(
        body["accept_url"],
        format!(
            "https://app.example.gov/invite/accept?invite={}",
            body["invite_id"].as_str().unwrap()
        )
    );
    assert_eq!(*sender.0.lock().unwrap(), 0);
    assert_eq!(store.0.lock().unwrap().len(), 1);
    let (status, _) = post(&app, "/users", serde_json::json!({"username":"direct@example.gov", "password":"passwordlong", "roles":["member"], "display_name":"Direct"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
#[tokio::test(flavor = "multi_thread")]
async fn create_user_permission_does_not_grant_invites() {
    let (app, store, _) = app(EmailDelivery::Link, "Create").await;
    let (status, body) = post(&app, "/auth/invites", invite("member", "org_a")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(store.0.lock().unwrap().is_empty());
}
#[tokio::test(flavor = "multi_thread")]
async fn cross_tenant_and_upward_role_grants_cannot_create_pending_invites() {
    let (app, store, sender) = app(EmailDelivery::Link, "Invite").await;
    for body in [
        invite("member", "org_b"),
        invite("admin", "org_a"),
        invite("platform_admin", "org_a"),
    ] {
        let (status, response) = post(&app, "/auth/invites", body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    }
    let mut invalid = invite("member", "org_a");
    invalid["tenant_type"] = "Unknown".into();
    assert_eq!(
        post(&app, "/auth/invites", invalid).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(store.0.lock().unwrap().is_empty());
    assert_eq!(*sender.0.lock().unwrap(), 0);
}
#[tokio::test(flavor = "multi_thread")]
async fn smtp_failure_returns_recoverable_safe_error_and_keeps_invitation_pending() {
    let (app, store, sender) = app(EmailDelivery::Smtp, "Invite").await;
    let (status, body) = post(&app, "/auth/invites", invite("member", "org_a")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"], "invite_delivery_failed");
    assert_eq!(body["delivery"], "failed");
    assert!(!body.to_string().contains("secret-password"));
    let invites = store.0.lock().unwrap();
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].status, InviteStatus::Pending);
    assert_eq!(body["invite_id"], invites[0].jti);
    assert!(body["accept_url"]
        .as_str()
        .unwrap()
        .contains(&invites[0].jti));
    assert_eq!(*sender.0.lock().unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn invite_only_owner_can_list_grantable_roles_without_listing_users() {
    let (app, _, _) = app(EmailDelivery::Link, "Invite").await;
    for (path, expected_status) in [
        ("/users/roles", StatusCode::OK),
        ("/users", StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        if path == "/users/roles" {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let names: Vec<_> = body["roles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, ["member", "owner"]);
        }
    }
}
