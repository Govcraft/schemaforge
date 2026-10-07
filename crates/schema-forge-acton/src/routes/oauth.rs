//! Public OAuth redirects and single-use login-code exchange.
use std::{collections::BTreeMap, sync::Arc};

use acton_service::{
    auth::{
        config::OAuthConfig,
        oauth::{
            MemoryOAuthStateManager, OAuthProvider, OAuthProviderRegistry, OAuthStateManager,
            StateData,
        },
        tokens::paseto_generator::PasetoGenerator,
    },
    middleware::PasetoAuth,
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::Utc;
use schema_forge_backend::{
    invite_store::InviteStore,
    oauth_identity::{ProviderIdentity, ProviderName},
    tenant::TenantConfig,
    user_store::ForgeUser,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};

use super::{
    audit::RequestAuditSource,
    auth::{
        emit_login_result, internal_error_response, unauthorized_response, LoginContext,
        LoginResponse, LoginSource,
    },
};
use crate::{
    authz::PrincipalClaimMappings,
    config::SchemaForgeConfig,
    error::ForgeError,
    invite::verify_invite_token,
    oauth_config::{OAuthSettings, SignupPolicy},
    state::DynAuthStore,
    tenancy_config::DefaultTenant,
};

const STATE_TTL_SECS: u64 = 600;
const LOGIN_CODE_TTL_SECS: u64 = 60;
const LOGIN_CODE_KIND: &str = "forge_login_code";

#[derive(Debug, Clone, Copy)]
enum CallbackFailure {
    EmailUnverified,
    InviteInvalid,
    InviteOnly,
    AccountExistsUnlinked,
    NoTenant,
    ProviderError,
}

impl CallbackFailure {
    fn code(self) -> &'static str {
        match self {
            Self::EmailUnverified => "email_unverified",
            Self::InviteInvalid => "invite_invalid",
            Self::InviteOnly => "invite_only",
            Self::AccountExistsUnlinked => "account_exists_unlinked",
            Self::NoTenant => "no_tenant",
            Self::ProviderError => "provider_error",
        }
    }
}

/// Shared login dependencies, constructed once by the embedding service.
#[derive(Clone)]
pub struct OAuthLoginServices {
    pub auth_store: Arc<dyn DynAuthStore>,
    pub generator: Arc<PasetoGenerator>,
    pub validator: Arc<PasetoAuth>,
    pub invites: Arc<dyn InviteStore>,
    pub principal_claims: Arc<PrincipalClaimMappings>,
    pub tenant_config: Arc<Option<TenantConfig>>,
}

enum Providers {
    Configured {
        config: OAuthConfig,
        registry: OnceCell<OAuthProviderRegistry>,
    },
    Injected(BTreeMap<String, Box<dyn OAuthProvider>>),
}

/// One bounded, process-local OAuth runtime, shared between all four handlers.
///
/// Redirect states live for ten minutes; exchanged login codes live for sixty
/// seconds. Both are consumed exactly once. This runtime does not support a
/// callback landing on a different process; use sticky routing for that flow.
pub struct OAuthRuntime {
    settings: OAuthSettings,
    providers: Providers,
    states: Arc<dyn OAuthStateManager>,
    login_codes: Arc<dyn OAuthStateManager>,
    provisioning: Mutex<()>,
    default_tenant: Option<DefaultTenant>,
}

impl OAuthRuntime {
    /// Validate all providers and return targets at service startup.
    /// The audited registry is initialized once the service audit logger exists.
    pub fn from_config(settings: OAuthSettings, config: OAuthConfig) -> Result<Self, ForgeError> {
        settings
            .validate()
            .map_err(|error| ForgeError::ValidationFailed {
                details: vec![error.to_string()],
            })?;
        for name in config.providers.keys() {
            ProviderName::new(name.clone()).map_err(|error| ForgeError::ValidationFailed {
                details: vec![error.to_string()],
            })?;
        }
        OAuthProviderRegistry::from_config(&config).map_err(|error| {
            ForgeError::ValidationFailed {
                details: vec![error.to_string()],
            }
        })?;
        Ok(Self {
            settings,
            providers: Providers::Configured {
                config,
                registry: OnceCell::new(),
            },
            states: Arc::new(MemoryOAuthStateManager::new(STATE_TTL_SECS)),
            login_codes: Arc::new(MemoryOAuthStateManager::new(LOGIN_CODE_TTL_SECS)),
            provisioning: Mutex::new(()),
            default_tenant: None,
        })
    }

    /// Inject providers and state managers for isolated integration tests or an
    /// application's own audited provider implementation. No network is needed.
    pub fn with_providers(
        settings: OAuthSettings,
        providers: BTreeMap<String, Box<dyn OAuthProvider>>,
        states: Arc<dyn OAuthStateManager>,
        login_codes: Arc<dyn OAuthStateManager>,
    ) -> Result<Self, ForgeError> {
        settings
            .validate()
            .map_err(|error| ForgeError::ValidationFailed {
                details: vec![error.to_string()],
            })?;
        for name in providers.keys() {
            ProviderName::new(name.clone()).map_err(|error| ForgeError::ValidationFailed {
                details: vec![error.to_string()],
            })?;
        }
        Ok(Self {
            settings,
            providers: Providers::Injected(providers),
            states,
            login_codes,
            provisioning: Mutex::new(()),
            default_tenant: None,
        })
    }

    /// Set the default root validated by the embedding application's startup.
    /// Signed invitation grants take precedence over this signup target.
    pub fn with_default_tenant(mut self, default_tenant: Option<DefaultTenant>) -> Self {
        self.default_tenant = default_tenant;
        self
    }

    async fn provider<'a>(
        &'a self,
        name: &str,
        state: &AppState<SchemaForgeConfig>,
    ) -> Result<Option<&'a dyn OAuthProvider>, ForgeError> {
        match &self.providers {
            Providers::Injected(providers) => Ok(providers.get(name).map(Box::as_ref)),
            Providers::Configured { config, registry } => {
                let registry = registry
                    .get_or_try_init(|| async {
                        if let Some(logger) = state.audit_logger() {
                            OAuthProviderRegistry::from_config_audited(config, logger.clone())
                        } else {
                            OAuthProviderRegistry::from_config(config)
                        }
                        .map_err(|error| ForgeError::Internal {
                            message: format!("OAuth registry failed: {error}"),
                        })
                    })
                    .await?;
                Ok(registry.get(name))
            }
        }
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = match &self.providers {
            Providers::Injected(providers) => providers.keys().cloned().collect(),
            Providers::Configured { config, .. } => config.providers.keys().cloned().collect(),
        };
        names.sort();
        names
    }
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub return_to: String,
    pub invite_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RedirectState {
    return_to: String,
    invite_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExchangeRequest {
    pub code: String,
}

/// GET /auth/oauth/{provider}/start.
pub async fn start(
    State(state): State<AppState<SchemaForgeConfig>>,
    Extension(runtime): Extension<Arc<OAuthRuntime>>,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
) -> Result<Response, ForgeError> {
    if !runtime.settings.enabled {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let Some(implementation) = runtime.provider(&provider, &state).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let return_to = runtime
        .settings
        .validate_return_to(&query.return_to)
        .map_err(|error| ForgeError::InvalidQuery {
            message: error.to_string(),
        })?;
    if query
        .invite_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 128)
    {
        return Err(ForgeError::InvalidQuery {
            message: "invalid invite_id".into(),
        });
    }
    let data = StateData {
        provider,
        redirect_uri: None,
        created_at: Utc::now().timestamp(),
        extra: Some(
            serde_json::to_value(RedirectState {
                return_to: return_to.as_str().into(),
                invite_id: query.invite_id,
            })
            .map_err(json_error)?,
        ),
    };
    let nonce = runtime
        .states
        .create_state(&data)
        .await
        .map_err(manager_error)?;
    redirect(implementation.authorization_url(&nonce, &[]))
}

/// GET /auth/oauth/providers returns stable, sorted configured names.
pub async fn providers(Extension(runtime): Extension<Arc<OAuthRuntime>>) -> Response {
    if !runtime.settings.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(runtime.names()).into_response()
}

/// GET /auth/oauth/{provider}/callback.
pub async fn callback(
    State(state): State<AppState<SchemaForgeConfig>>,
    RequestAuditSource(source): RequestAuditSource,
    Extension(runtime): Extension<Arc<OAuthRuntime>>,
    Extension(services): Extension<OAuthLoginServices>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if !runtime.settings.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    match finish_callback(&state, &source, &runtime, &services, &provider, query).await {
        Ok(response) => response,
        Err(response) => {
            emit_login_result(
                &state,
                "anonymous",
                &source,
                LoginSource::OAuth(provider.clone()),
                false,
            )
            .await;
            response
        }
    }
}

async fn finish_callback(
    state: &AppState<SchemaForgeConfig>,
    source: &acton_service::audit::AuditSource,
    runtime: &OAuthRuntime,
    services: &OAuthLoginServices,
    provider: &str,
    query: CallbackQuery,
) -> Result<Response, Response> {
    let Some(implementation) = runtime
        .provider(provider, state)
        .await
        .map_err(IntoResponse::into_response)?
    else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    let nonce = query
        .state
        .filter(|value| !value.is_empty())
        .ok_or_else(unauthorized_response)?;
    let data = runtime
        .states
        .validate_state(&nonce)
        .await
        .map_err(|_| unauthorized_response())?;
    if data.provider != provider {
        return Err(unauthorized_response());
    }
    let redirect_state: RedirectState =
        serde_json::from_value(data.extra.ok_or_else(unauthorized_response)?)
            .map_err(|_| unauthorized_response())?;
    let destination = runtime
        .settings
        .validate_return_to(&redirect_state.return_to)
        .map_err(|_| unauthorized_response())?;
    let result: Result<Response, CallbackFailure> = async {
        let code = query
            .code
            .filter(|value| !value.is_empty())
            .ok_or(CallbackFailure::ProviderError)?;
        let tokens = implementation
            .exchange_code(&code)
            .await
            .map_err(|_| CallbackFailure::ProviderError)?;
        let info = implementation
            .get_user_info(&tokens.access_token)
            .await
            .map_err(|_| CallbackFailure::ProviderError)?;
        let email = info
            .email
            .as_deref()
            .filter(|email| info.email_verified && valid_email(email))
            .ok_or(CallbackFailure::EmailUnverified)?;
        let identity = ProviderIdentity::new(provider, info.provider_user_id)
            .map_err(|_| CallbackFailure::ProviderError)?;

        // Serialize new-account/invite provisioning within the process. Backend
        // uniqueness still protects identity links against external writers.
        let user = {
            let _guard = runtime.provisioning.lock().await;
            match services
                .auth_store
                .find_user_by_identity(&identity)
                .await
                .map_err(|_| CallbackFailure::ProviderError)?
            {
                Some(user) => user,
                None => {
                    provision_account(
                        runtime,
                        services,
                        &identity,
                        email,
                        info.name.as_deref(),
                        redirect_state.invite_id.as_deref(),
                    )
                    .await?
                }
            }
        };
        let context = LoginContext {
            state,
            source,
            auth_store: services.auth_store.as_ref(),
            generator: &services.generator,
            principal_claims: &services.principal_claims,
            tenant_config: services.tenant_config.as_ref().as_ref(),
        };
        let login = context
            .complete(&user, LoginSource::OAuth(provider.to_owned()))
            .await
            .map_err(|failure| match failure {
                super::auth::LoginFailure::NoTenantAssigned => CallbackFailure::NoTenant,
                super::auth::LoginFailure::Response(_) => CallbackFailure::ProviderError,
            })?;
        let payload = StateData {
            provider: LOGIN_CODE_KIND.into(),
            redirect_uri: None,
            created_at: Utc::now().timestamp(),
            extra: Some(serde_json::to_value(login).map_err(|_| CallbackFailure::ProviderError)?),
        };
        let login_code = runtime
            .login_codes
            .create_state(&payload)
            .await
            .map_err(|_| CallbackFailure::ProviderError)?;
        redirect(destination.clone().with_login_code(&login_code))
            .map_err(|_| CallbackFailure::ProviderError)
    }
    .await;
    result.map_err(|error| {
        redirect(destination.with_error(error.code())).unwrap_or_else(IntoResponse::into_response)
    })
}

async fn provision_account(
    runtime: &OAuthRuntime,
    services: &OAuthLoginServices,
    identity: &ProviderIdentity,
    email: &str,
    display_name: Option<&str>,
    invite_id: Option<&str>,
) -> Result<ForgeUser, CallbackFailure> {
    // Never auto-link a provider to an existing account by matching its email.
    if services
        .auth_store
        .get_user(email)
        .await
        .map_err(|_| CallbackFailure::ProviderError)?
        .is_some()
    {
        return Err(CallbackFailure::AccountExistsUnlinked);
    }
    let invite = if let Some(id) = invite_id {
        let invite = services
            .invites
            .find_by_jti(id)
            .await
            .map_err(|_| CallbackFailure::ProviderError)?
            .ok_or(CallbackFailure::InviteInvalid)?;
        if !invite.is_acceptable(Utc::now()) {
            return Err(CallbackFailure::InviteInvalid);
        }
        let verified = verify_invite_token(services.validator.as_ref(), &invite.token)
            .map_err(|_| CallbackFailure::InviteInvalid)?;
        if verified.invite_id != invite.jti || verified.email != email {
            return Err(CallbackFailure::InviteInvalid);
        }
        Some((invite, verified))
    } else {
        None
    };
    let roles = match &invite {
        Some((_, verified)) => verified
            .role
            .iter()
            .filter(|role| !role.is_empty())
            .cloned()
            .collect(),
        None if runtime.settings.signup == SignupPolicy::Open => {
            runtime.settings.default_roles.clone()
        }
        None => return Err(CallbackFailure::InviteOnly),
    };
    let display_name = invite
        .as_ref()
        .and_then(|(row, _)| row.display_name.as_deref())
        .or(display_name)
        .unwrap_or(email);
    services
        .auth_store
        .create_user_without_password(email, &roles, display_name)
        .await
        .map_err(|_| CallbackFailure::ProviderError)?;
    if let Some((_, verified)) = &invite {
        if let (Some(tenant_type), Some(tenant_id)) = (&verified.tenant_type, &verified.tenant_id) {
            services
                .auth_store
                .add_tenant_membership(email, tenant_type, tenant_id, verified.role.as_deref())
                .await
                .map_err(|_| CallbackFailure::ProviderError)?;
        }
    }
    if invite.is_none() {
        if let Some(default) = &runtime.default_tenant {
            services
                .auth_store
                .add_tenant_membership(
                    email,
                    default.schema.as_str(),
                    default.id.as_str(),
                    Some(&default.role),
                )
                .await
                .map_err(|_| CallbackFailure::ProviderError)?;
        }
    }
    services
        .auth_store
        .link_identity(email, identity, email)
        .await
        .map_err(|_| CallbackFailure::ProviderError)?;
    // As with password invitations, consumption follows successful account,
    // membership and identity writes. No provider credentials are persisted.
    if let Some((invite, _)) = &invite {
        services
            .invites
            .mark_consumed(&invite.id, Utc::now())
            .await
            .map_err(|_| CallbackFailure::ProviderError)?;
    }
    services
        .auth_store
        .get_user(email)
        .await
        .map_err(|_| CallbackFailure::ProviderError)?
        .ok_or(CallbackFailure::ProviderError)
}

/// POST /auth/oauth/exchange consumes one opaque code and returns LoginResponse.
pub async fn exchange(
    Extension(runtime): Extension<Arc<OAuthRuntime>>,
    Extension(services): Extension<OAuthLoginServices>,
    Json(request): Json<ExchangeRequest>,
) -> Response {
    if !runtime.settings.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let data = match runtime.login_codes.validate_state(&request.code).await {
        Ok(data) if data.provider == LOGIN_CODE_KIND => data,
        _ => return unauthorized_response(),
    };
    let Some(extra) = data.extra else {
        return unauthorized_response();
    };
    let response: LoginResponse = match serde_json::from_value(extra) {
        Ok(response) => response,
        Err(_) => return unauthorized_response(),
    };
    // Do not hand out a token if the account was disabled/deleted after its
    // callback. Live grants are rebuilt by the ordinary refresh endpoint.
    use acton_service::middleware::token::TokenValidator;
    let claims = match services.validator.validate_token(&response.token) {
        Ok(claims) => claims,
        Err(_) => return unauthorized_response(),
    };
    let username = claims.sub.strip_prefix("user:").unwrap_or(&claims.sub);
    match services.auth_store.get_user(username).await {
        Ok(Some(user)) if user.active => {
            let mut result = Json(response).into_response();
            result.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            result
        }
        Ok(_) => unauthorized_response(),
        Err(error) => store_error(error),
    }
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && email.len() <= 512
        && !email
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

fn redirect(location: String) -> Result<Response, ForgeError> {
    let location =
        axum::http::HeaderValue::from_str(&location).map_err(|_| ForgeError::Internal {
            message: "invalid OAuth redirect URL".into(),
        })?;
    let mut response = StatusCode::FOUND.into_response();
    response
        .headers_mut()
        .insert(axum::http::header::LOCATION, location);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}
fn store_error(error: schema_forge_backend::BackendError) -> Response {
    internal_error_response(format!("auth store error: {error}"))
}
fn json_error(error: serde_json::Error) -> ForgeError {
    ForgeError::Internal {
        message: format!("OAuth state serialization failed: {error}"),
    }
}
fn manager_error(error: acton_service::error::Error) -> ForgeError {
    ForgeError::Internal {
        message: format!("OAuth state storage failed: {error}"),
    }
}

/// Merge only when enabled; omitted features/configuration leave a 404 route.
pub fn oauth_routes() -> Router<AppState<SchemaForgeConfig>> {
    Router::new()
        .route("/auth/oauth/providers", get(providers))
        .route("/auth/oauth/exchange", post(exchange))
        .route("/auth/oauth/{provider}/start", get(start))
        .route("/auth/oauth/{provider}/callback", get(callback))
}
