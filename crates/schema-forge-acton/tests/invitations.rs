//! Invitation policy and delivery regressions for issue 208.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use acton_service::auth::config::TokenGenerationConfig;
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::config::{Config, PasetoConfig};
use acton_service::middleware::paseto::PasetoAuth;
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
    BackendError, EntityAuthStore, ForgeInvitation, InvitationListQuery, InvitationPage,
    InviteStatus, InviteStore, NewInvitation,
};
use schema_forge_core::types::{EntityId, SchemaName};
use tokio::sync::oneshot;
use tower::ServiceExt;

#[derive(Default)]
struct MemoryInvites(Mutex<Vec<ForgeInvitation>>, AtomicBool);
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
    async fn find_by_id(&self, id: &EntityId) -> Result<Option<ForgeInvitation>, BackendError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|invite| &invite.id == id)
            .cloned())
    }
    async fn list_pending(
        &self,
        query: &InvitationListQuery,
    ) -> Result<InvitationPage, BackendError> {
        let rows = self.0.lock().unwrap();
        let mut matching: Vec<_> = rows
            .iter()
            .filter(|invite| {
                invite.is_acceptable(query.at)
                    && query.tenant.as_ref().is_none_or(|tenant| {
                        invite.tenant_type.as_deref() == Some(tenant.schema.as_str())
                            && invite.tenant_id.as_deref() == Some(tenant.entity_id.as_str())
                    })
            })
            .cloned()
            .collect();
        matching.sort_by(|a, b| a.id.cmp(&b.id));
        let next_offset =
            (matching.len() > query.offset + query.limit).then_some(query.offset + query.limit);
        Ok(InvitationPage {
            invitations: matching
                .into_iter()
                .skip(query.offset)
                .take(query.limit)
                .collect(),
            next_offset,
        })
    }
    async fn try_consume(&self, id: &EntityId, at: DateTime<Utc>) -> Result<bool, BackendError> {
        let mut rows = self.0.lock().unwrap();
        let Some(invite) = rows.iter_mut().find(|invite| &invite.id == id) else {
            return Ok(false);
        };
        // Deterministically model revocation after the route's initial read.
        if self.1.swap(false, Ordering::SeqCst) {
            invite.status = InviteStatus::Revoked;
        }
        if !invite.is_acceptable(at) {
            return Ok(false);
        }
        invite.status = InviteStatus::Consumed;
        invite.consumed_at = Some(at);
        Ok(true)
    }
    async fn revoke(&self, id: &EntityId, _at: DateTime<Utc>) -> Result<bool, BackendError> {
        let mut rows = self.0.lock().unwrap();
        let Some(invite) = rows.iter_mut().find(|invite| &invite.id == id) else {
            return Ok(false);
        };
        if invite.status != InviteStatus::Pending {
            return Ok(false);
        }
        invite.status = InviteStatus::Revoked;
        Ok(true)
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

struct InvitationApp {
    router: Router,
    store: Arc<MemoryInvites>,
    sender: Arc<FailingEmail>,
    auth: Arc<dyn DynAuthStore>,
}

async fn app(
    delivery: EmailDelivery,
    action: &str,
) -> (Router, Arc<MemoryInvites>, Arc<FailingEmail>) {
    let fixture = invitation_app(delivery, &[&format!("{action}User")], "owner").await;
    (fixture.router, fixture.store, fixture.sender)
}

async fn invitation_app(delivery: EmailDelivery, actions: &[&str], role: &str) -> InvitationApp {
    invitation_app_with_policy(delivery, actions, role, "").await
}

async fn invitation_app_with_policy(
    delivery: EmailDelivery,
    actions: &[&str],
    role: &str,
    extra_policy: &str,
) -> InvitationApp {
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("invites", "invites")
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
    let grants = actions
        .iter()
        .map(|action| {
            format!(
                r#"
permit(principal in Forge::Group::"owner", action == Action::"{action}", resource is User)
when {{ resource has "_tenant" && principal in resource["_tenant"] }};
"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        custom.path().join("invite.cedar"),
        format!("{grants}\n{extra_policy}"),
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
        "sub": "owner@example.gov", "roles": [role], "perms": [], "exp": 9999999999_u64
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
    let key = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(key.path(), [11; 32]).unwrap();
    let validator = Arc::new(
        PasetoAuth::new(&PasetoConfig {
            version: "v4".into(),
            purpose: "local".into(),
            key_path: key.path().into(),
            issuer: None,
            audience: None,
            public_paths: Vec::new(),
        })
        .unwrap(),
    );
    let app = forge_routes()
        .merge(schema_forge_acton::routes::auth::auth_routes())
        .layer(Extension(auth_store.clone()))
        .layer(Extension(validator))
        .layer(Extension(invite_store))
        .layer(Extension(email_sender))
        .layer(Extension(generator))
        .layer(Extension(claims))
        .with_state(service.state().clone());
    InvitationApp {
        router: app,
        store,
        sender,
        auth: auth_store,
    }
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

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    tenant: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(tenant) = tenant {
        request = request.header("x-active-tenant", tenant);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!(String::from_utf8_lossy(&bytes)))
    };
    (status, body)
}

fn pending(tenant: &str, email: &str, role: &str) -> ForgeInvitation {
    ForgeInvitation {
        id: EntityId::new("forgeinvitation"),
        email: email.into(),
        display_name: None,
        tenant_type: Some("Organization".into()),
        tenant_id: Some(tenant.into()),
        role: Some(role.into()),
        jti: format!("private-jti-{email}"),
        token: format!("private-token-{email}"),
        status: InviteStatus::Pending,
        expires_at: Some(Utc::now() + chrono::Duration::days(7)),
        invited_by: Some("owner@example.gov".into()),
        consumed_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_lists_safe_pending_rows_and_revokes_without_exposing_acceptance_credentials() {
    let fixture = invitation_app(
        EmailDelivery::Link,
        &["InviteUser", "ListInvites", "RevokeInvite"],
        "owner",
    )
    .await;
    let (status, created) = post(&fixture.router, "/auth/invites", invite("owner", "org_a")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let own = fixture.store.0.lock().unwrap()[0].clone();
    let mut expired = pending("org_a", "expired@example.gov", "member");
    expired.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    let mut revoked = pending("org_a", "revoked@example.gov", "member");
    revoked.status = InviteStatus::Revoked;
    let mut consumed = pending("org_a", "consumed@example.gov", "member");
    consumed.status = InviteStatus::Consumed;
    fixture.store.0.lock().unwrap().extend([
        pending("org_b", "other@example.gov", "member"),
        expired,
        revoked,
        consumed,
    ]);
    let (status, listed) = request(&fixture.router, "GET", "/auth/invites", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let rows = listed["invitations"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let mut fields: Vec<_> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["created_at", "email", "expires_at", "id", "inviter", "role"]
    );
    assert_eq!(rows[0]["id"], own.id.as_str());
    assert_eq!(rows[0]["role"], "owner");
    assert_eq!(
        rows[0]["created_at"],
        serde_json::to_value(own.id.created_at()).unwrap()
    );
    let encoded = listed.to_string();
    assert!(!encoded.contains(&own.token));
    assert!(!encoded.contains(&own.jti));
    assert!(!encoded.contains("other@example.gov"));
    let (status, malformed) = request(
        &fixture.router,
        "DELETE",
        "/auth/invites/v4.local.SECRET-PASETO",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!malformed.to_string().contains("SECRET-PASETO"));
    let path = format!("/auth/invites/{}", own.id);
    for _ in 0..2 {
        assert_eq!(
            request(&fixture.router, "DELETE", &path, None).await,
            (StatusCode::NO_CONTENT, serde_json::Value::Null)
        );
    }
    let accepted = post(
        &fixture.router,
        "/auth/invites/accept",
        serde_json::json!({"invite_id":own.jti,"password":"secure-password"}),
    )
    .await;
    assert_eq!(
        accepted.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        accepted.1
    );
    assert_eq!(
        accepted.1["message"],
        "validation failed: invitation is expired or already used"
    );
    assert!(fixture.auth.get_user(&own.email).await.unwrap().is_none());
    assert_eq!(
        request(&fixture.router, "GET", "/auth/invites", None)
            .await
            .1["invitations"],
        serde_json::json!([])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn owners_cannot_change_active_scope_or_distinguish_foreign_invitation_rows() {
    let fixture = invitation_app(
        EmailDelivery::Link,
        &["ListInvites", "RevokeInvite"],
        "owner",
    )
    .await;
    let foreign = pending("org_b", "other@example.gov", "owner");
    fixture.store.0.lock().unwrap().push(foreign.clone());
    let (status, _) = request(
        &fixture.router,
        "GET",
        "/auth/invites",
        Some("Organization:org_b"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(
        &fixture.router,
        "DELETE",
        &format!("/auth/invites/{}", foreign.id),
        Some("Organization:org_b"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let foreign_response = request(
        &fixture.router,
        "DELETE",
        &format!("/auth/invites/{}", foreign.id),
        None,
    )
    .await;
    let absent_response = request(
        &fixture.router,
        "DELETE",
        &format!("/auth/invites/{}", EntityId::new("forgeinvitation")),
        None,
    )
    .await;
    assert_eq!(foreign_response.0, StatusCode::NOT_FOUND);
    assert_eq!(absent_response.0, foreign_response.0);
    assert_eq!(foreign_response.1["error"], absent_response.1["error"]);
    assert_eq!(
        fixture.store.0.lock().unwrap()[0].status,
        InviteStatus::Pending
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_and_revoke_permissions_are_independent_of_invitation_creation_and_user_crud() {
    for actions in [
        &["InviteUser"][..],
        &["ListUser", "DeleteUser"][..],
        &["ListInvites"][..],
        &["RevokeInvite"][..],
    ] {
        let fixture = invitation_app(EmailDelivery::Link, actions, "owner").await;
        let row = pending("org_a", "new@example.gov", "member");
        fixture.store.0.lock().unwrap().push(row.clone());
        assert_eq!(
            request(&fixture.router, "GET", "/auth/invites", None)
                .await
                .0,
            if actions.contains(&"ListInvites") {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            request(
                &fixture.router,
                "DELETE",
                &format!("/auth/invites/{}", row.id),
                None
            )
            .await
            .0,
            if actions.contains(&"RevokeInvite") {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    let fixture = invitation_app(EmailDelivery::Link, &["InviteUser"], "owner").await;
    assert_eq!(
        request(&fixture.router, "GET", "/auth/invites", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn platform_admin_can_list_and_revoke_across_tenants_or_select_one() {
    let fixture = invitation_app(EmailDelivery::Link, &[], "platform_admin").await;
    let first = pending("org_a", "first@example.gov", "owner");
    let tenant = EntityId::new("organization");
    let second = pending(tenant.as_str(), "second@example.gov", "owner");
    fixture
        .store
        .0
        .lock()
        .unwrap()
        .extend([first, second.clone()]);
    let (status, body) = request(&fixture.router, "GET", "/auth/invites", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["invitations"].as_array().unwrap().len(), 2);
    let header = format!("Organization:{tenant}");
    let (status, body) = request(&fixture.router, "GET", "/auth/invites", Some(&header)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["invitations"].as_array().unwrap().len(), 1);
    assert_eq!(body["invitations"][0]["id"], second.id.as_str());
    assert_eq!(
        request(
            &fixture.router,
            "DELETE",
            &format!("/auth/invites/{}", second.id),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    for header in [
        "Organization:not-an-id",
        "Unknown:organization_01k75mebsxe41tzcn0jnkmfnvv",
        "garbage",
    ] {
        assert_eq!(
            request(&fixture.router, "GET", "/auth/invites", Some(header))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pagination_is_bounded_and_preserves_custom_per_invitation_denials() {
    let fixture = invitation_app_with_policy(
        EmailDelivery::Link,
        &["ListInvites", "RevokeInvite"],
        "owner",
        r#"forbid(principal, action in [Action::"ListInvites", Action::"RevokeInvite"], resource is User)
when { resource.role_rank > 20 };"#,
    )
    .await;
    let too_privileged = pending("org_a", "admin@example.gov", "admin");
    fixture.store.0.lock().unwrap().extend([
        pending("org_a", "first@example.gov", "member"),
        pending("org_a", "second@example.gov", "owner"),
        too_privileged.clone(),
    ]);
    let (status, page) = request(&fixture.router, "GET", "/auth/invites?limit=1", None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page["invitations"].as_array().unwrap().len() <= 1);
    assert_eq!(page["next_offset"], 1);
    let (status, page) = request(&fixture.router, "GET", "/auth/invites?limit=100", None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["invitations"].as_array().unwrap().len(), 2);
    assert!(!page.to_string().contains("admin@example.gov"));
    assert_eq!(
        request(
            &fixture.router,
            "DELETE",
            &format!("/auth/invites/{}", too_privileged.id),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for query in [
        "limit=0",
        "limit=101",
        "limit=-1",
        "limit=bad",
        "offset=1000001",
        "offset=-1",
        "tenant_id=org_b",
        "limit=1&limit=2",
    ] {
        assert_eq!(
            request(
                &fixture.router,
                "GET",
                &format!("/auth/invites?{query}"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_between_password_validation_and_claim_prevents_all_provisioning() {
    let fixture = invitation_app(EmailDelivery::Link, &["InviteUser"], "owner").await;
    let (status, body) = post(&fixture.router, "/auth/invites", invite("member", "org_a")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    fixture.store.1.store(true, Ordering::SeqCst);
    let (status, body) = post(
        &fixture.router,
        "/auth/invites/accept",
        serde_json::json!({"invite_id":body["invite_id"], "password":"secure-password"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(fixture
        .auth
        .get_user("new@example.gov")
        .await
        .unwrap()
        .is_none());
    assert!(fixture
        .auth
        .list_tenant_memberships("new@example.gov")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture.store.0.lock().unwrap()[0].status,
        InviteStatus::Revoked
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_membership_provisioning_burns_claim_and_cannot_be_revoked_or_replayed() {
    // This fixture deliberately omits TenantMembership storage.
    let fixture = invitation_app(
        EmailDelivery::Link,
        &["InviteUser", "RevokeInvite"],
        "owner",
    )
    .await;
    let (status, body) = post(&fixture.router, "/auth/invites", invite("member", "org_a")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let acceptance =
        serde_json::json!({"invite_id":body["invite_id"], "password":"secure-password"});
    assert_eq!(
        post(&fixture.router, "/auth/invites/accept", acceptance.clone())
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(fixture
        .auth
        .get_user("new@example.gov")
        .await
        .unwrap()
        .is_some());
    let row = fixture.store.0.lock().unwrap()[0].clone();
    assert_eq!(row.status, InviteStatus::Consumed);
    assert_eq!(
        request(
            &fixture.router,
            "DELETE",
            &format!("/auth/invites/{}", row.id),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&fixture.router, "/auth/invites/accept", acceptance)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_can_manage_higher_role_invitations_in_own_tenant_without_granting_that_role() {
    let fixture = invitation_app(
        EmailDelivery::Link,
        &["ListInvites", "RevokeInvite"],
        "owner",
    )
    .await;
    let row = pending("org_a", "admin@example.gov", "admin");
    fixture.store.0.lock().unwrap().push(row.clone());
    let (status, listed) = request(&fixture.router, "GET", "/auth/invites", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["invitations"][0]["id"], row.id.as_str());
    assert_eq!(
        request(
            &fixture.router,
            "DELETE",
            &format!("/auth/invites/{}", row.id),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_password_acceptance_provisions_one_account_and_consumes_once() {
    let fixture = invitation_app(EmailDelivery::Link, &[], "platform_admin").await;
    let (status, body) = post(
        &fixture.router,
        "/auth/invites",
        serde_json::json!({
            "email":"new@example.gov", "role":"member"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let acceptance =
        serde_json::json!({"invite_id":body["invite_id"], "password":"secure-password"});
    let (first, second) = tokio::join!(
        post(&fixture.router, "/auth/invites/accept", acceptance.clone()),
        post(&fixture.router, "/auth/invites/accept", acceptance),
    );
    let statuses = [first.0, second.0];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CREATED)
            .count(),
        1,
        "{first:?} {second:?}"
    );
    assert!(
        statuses.iter().any(|status| matches!(
            *status,
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::CONFLICT
        )),
        "{first:?} {second:?}"
    );
    let user = fixture
        .auth
        .get_user("new@example.gov")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.roles, ["member"]);
    assert_eq!(fixture.auth.list_users().await.unwrap().len(), 1);
    let rows = fixture.store.0.lock().unwrap();
    assert_eq!(rows[0].status, InviteStatus::Consumed);
    assert!(rows[0].consumed_at.is_some());
}
