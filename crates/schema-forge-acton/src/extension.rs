use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;

use crate::error::ForgeError;
use crate::routes::forge_routes;
use crate::state::{
    DynAuthStore, DynEntityStore, DynForgeBackend, DynSchemaBackend, ForgeState, SchemaRegistry,
};

use acton_service::session::{MemoryStore, SessionManagerLayer};
use acton_service::state::AppState;
use crate::config::SchemaForgeConfig;
use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::types::SchemaDefinition;

// ---------------------------------------------------------------------------
// InitForgeData — bundle of data needed to initialize a ForgeActor
// ---------------------------------------------------------------------------

/// Bundle of initialization data for the `ForgeActor`.
///
/// Produced by [`SchemaForgeExtension::build_init`] and consumed by sending
/// an [`InitForge`](crate::messages::InitForge) message after actor spawning.
pub struct InitForgeData {
    /// Pre-loaded schema registry (HashMap, not the async SchemaRegistry).
    pub registry: HashMap<String, SchemaDefinition>,
    /// The backend for schema and entity operations.
    pub backend: Arc<dyn DynForgeBackend>,
    /// Tenant configuration derived from schema annotations.
    pub tenant_config: Option<TenantConfig>,
    /// Optional record-level access policy.
    pub record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
}

// ---------------------------------------------------------------------------
// SchemaForgeExtension
// ---------------------------------------------------------------------------

/// Builder for SchemaForge's acton-service integration.
///
/// Usage:
/// ```rust,ignore
/// let extension = SchemaForgeExtension::builder()
///     .with_backend(surreal_backend)
///     .build()
///     .await?;
///
/// // Then in VersionedApiBuilder:
/// let routes = VersionedApiBuilder::<SchemaForgeConfig>::with_config()
///     .with_base_path("/api")
///     .add_version(ApiVersion::V1, |router| {
///         extension.register_routes(router)
///     })
///     .build_routes();
/// ```
pub struct SchemaForgeExtension {
    state: ForgeState,
    session_layer: SessionManagerLayer<MemoryStore>,
}

/// Builder for `SchemaForgeExtension`.
pub struct SchemaForgeExtensionBuilder {
    backend: Option<Arc<dyn DynForgeBackend>>,
    record_access_policy: Option<Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>>,
    auth_store: Option<Arc<dyn DynAuthStore>>,
    admin_credentials: Option<(String, String)>,
    template_dir: Option<std::path::PathBuf>,
    site_template_dir: Option<std::path::PathBuf>,
    webhook_config: crate::webhook::WebhookConfig,
}

impl SchemaForgeExtensionBuilder {
    /// Create a new builder.
    fn new() -> Self {
        Self {
            backend: None,
            record_access_policy: None,
            auth_store: None,
            admin_credentials: None,
            template_dir: None,
            site_template_dir: None,
            webhook_config: crate::webhook::WebhookConfig::default(),
        }
    }

    /// Set the backend for schema and entity operations.
    ///
    /// The backend must implement both `SchemaBackend` and `EntityStore`.
    pub fn with_backend<B>(mut self, backend: B) -> Self
    where
        B: DynSchemaBackend + DynEntityStore + 'static,
    {
        self.backend = Some(Arc::new(backend));
        self
    }

    /// Set the record-level access policy.
    ///
    /// When configured, entity handlers will check ownership before allowing
    /// modifications and deletions, and will filter list results based on
    /// the policy.
    pub fn with_record_access_policy<
        P: schema_forge_backend::auth::RecordAccessPolicy + 'static,
    >(
        mut self,
        policy: P,
    ) -> Self {
        self.record_access_policy = Some(Arc::new(policy));
        self
    }

    /// Set the auth store for user authentication and management.
    ///
    /// The auth store handles credential validation, user CRUD, and password
    /// hashing. Both `SurrealBackend` and `PgBackend` implement `AuthStore`.
    pub fn with_auth_store<A: schema_forge_backend::AuthStore + 'static>(
        mut self,
        store: A,
    ) -> Self {
        self.auth_store = Some(Arc::new(store));
        self
    }

    /// Set the backend from a pre-existing `Arc<dyn DynForgeBackend>`.
    ///
    /// Use this when the backend has already been connected and type-erased
    /// (e.g. in the CLI serve command where the concrete type is no longer available).
    pub fn with_backend_arc(mut self, backend: Arc<dyn DynForgeBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Set the auth store from a pre-existing `Arc<dyn DynAuthStore>`.
    ///
    /// Use this when the auth store has already been type-erased at connection time.
    pub fn with_auth_store_arc(mut self, store: Arc<dyn DynAuthStore>) -> Self {
        self.auth_store = Some(store);
        self
    }

    /// Set bootstrap credentials for the initial admin user.
    ///
    /// If the `_forge_users` table is empty during `build()`, an admin user
    /// will be created with these credentials.
    pub fn with_admin_credentials(mut self, username: String, password: String) -> Self {
        self.admin_credentials = Some((username, password));
        self
    }

    /// Set the directory for admin MiniJinja templates.
    ///
    /// Admin templates are loaded from this directory.
    /// Widget/forge templates are always embedded in the binary.
    pub fn with_template_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.template_dir = Some(dir);
        self
    }

    /// Set the directory for user-customizable site templates.
    ///
    /// Site templates are loaded from this directory first. If a template
    /// is not found on the filesystem, the embedded default is used.
    pub fn with_site_template_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.site_template_dir = Some(dir);
        self
    }

    /// Set the webhook configuration.
    pub fn with_webhook_config(mut self, config: crate::webhook::WebhookConfig) -> Self {
        self.webhook_config = config;
        self
    }

    /// Build the `SchemaForgeExtension`.
    ///
    /// Loads existing schemas from the backend into the in-memory registry.
    /// Returns an error if no backend was provided or if loading fails.
    pub async fn build(self) -> Result<SchemaForgeExtension, ForgeError> {
        let backend = self.backend.ok_or_else(|| ForgeError::Internal {
            message: "SchemaForgeExtensionBuilder requires a backend (call .with_backend())"
                .to_string(),
        })?;

        let registry = SchemaRegistry::new();

        // Load existing schemas from the backend
        registry
            .load_from_backend(backend.as_ref())
            .await
            .map_err(ForgeError::from)?;

        // Seed system schemas (idempotent)
        crate::system::seed_system_schemas(&registry, backend.as_ref()).await?;

        // Bootstrap admin user if configured
        if let (Some(ref auth_store), Some((ref username, ref password))) =
            (&self.auth_store, &self.admin_credentials)
        {
            crate::shared_auth::bootstrap_admin(auth_store.as_ref(), username, password)
                .await
                .map_err(|e| ForgeError::Internal {
                    message: format!("Admin bootstrap failed: {e}"),
                })?;
            crate::shared_auth::bootstrap_demo_users(auth_store.as_ref())
                .await
                .map_err(|e| ForgeError::Internal {
                    message: format!("Demo user bootstrap failed: {e}"),
                })?;
        }

        // Build tenant config from all registered schemas
        let all_schemas = registry.list().await;
        let tenant_config = schema_forge_backend::tenant::TenantConfig::from_schemas(&all_schemas)
            .map_err(|e| ForgeError::Internal {
                message: format!("Invalid tenant configuration: {e}"),
            })?;
        let tenant_config = if tenant_config.is_enabled() {
            Some(tenant_config)
        } else {
            None
        };

        // Build initial GraphQL schema
        #[cfg(feature = "graphql")]
        let graphql_schema = {
            let gql_schema = crate::graphql::build_initial_schema(&all_schemas)?;
            Arc::new(arc_swap::ArcSwap::new(Arc::new(gql_schema)))
        };

        // Construct MiniJinja template engine.
        // Widget/forge/shared templates are always embedded in the binary.
        // Admin templates are loaded from the filesystem when a template dir is provided.
        let template_engine = Arc::new(crate::template_engine::TemplateEngine::new(
            self.template_dir,
            self.site_template_dir,
        ));

        // Initialize webhook dispatcher if enabled
        let webhook_dispatcher = if self.webhook_config.enabled {
            tracing::info!("webhook system enabled");
            Some(Arc::new(crate::webhook::WebhookDispatcher::new(
                self.webhook_config,
            )))
        } else {
            None
        };

        let state = ForgeState {
            registry,
            backend,
            tenant_config,
            record_access_policy: self.record_access_policy,
            #[cfg(feature = "graphql")]
            graphql_schema,
            auth_store: self.auth_store,
            template_engine,
            webhook_dispatcher,
        };

        let session_config = acton_service::session::SessionConfig {
            secure: false,
            cookie_name: "forge_session".to_string(),
            ..Default::default()
        };
        let session_layer = acton_service::session::create_memory_session_layer(&session_config);

        Ok(SchemaForgeExtension {
            state,
            session_layer,
        })
    }
}

impl SchemaForgeExtension {
    /// Create a new builder for `SchemaForgeExtension`.
    pub fn builder() -> SchemaForgeExtensionBuilder {
        SchemaForgeExtensionBuilder::new()
    }

    /// Build initialization data for a `ForgeActor`.
    ///
    /// This is the actor-model alternative to `builder().build()`. Instead of
    /// constructing a `ForgeState`, it returns an [`InitForgeData`] bundle that
    /// should be sent to the actor as an [`InitForge`](crate::messages::InitForge)
    /// message after it has been spawned by `ServiceBuilder::with_actor::<ForgeActor>()`.
    ///
    /// # Flow
    ///
    /// 1. Call `build_init(backend, ...)` to load schemas, seed system schemas,
    ///    and build tenant config.
    /// 2. Register the actor: `ServiceBuilder::new().with_actor::<ForgeActor>().build()`
    /// 3. Send `InitForge { registry, backend, ... }` to the actor.
    /// 4. Serve.
    pub async fn build_init(
        backend: Arc<dyn DynForgeBackend>,
        record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
    ) -> Result<InitForgeData, ForgeError> {
        // Load existing schemas from the backend into a HashMap
        let stored_schemas = backend
            .list_schema_metadata()
            .await
            .map_err(ForgeError::from)?;
        let mut registry: HashMap<String, SchemaDefinition> = stored_schemas
            .into_iter()
            .map(|s| (s.name.as_str().to_string(), s))
            .collect();

        // Seed system schemas (idempotent)
        crate::system::seed_system_schemas_into_map(&mut registry, backend.as_ref()).await?;

        // Build tenant config from all registered schemas
        let all_schemas: Vec<SchemaDefinition> = registry.values().cloned().collect();
        let tenant_config = TenantConfig::from_schemas(&all_schemas).map_err(|e| {
            ForgeError::Internal {
                message: format!("Invalid tenant configuration: {e}"),
            }
        })?;
        let tenant_config = if tenant_config.is_enabled() {
            Some(tenant_config)
        } else {
            None
        };

        Ok(InitForgeData {
            registry,
            backend,
            tenant_config,
            record_access_policy,
        })
    }

    /// Register SchemaForge routes onto an existing Router.
    ///
    /// Merges the forge routes (nested under `/forge`). Route handlers access
    /// the `ForgeActor` via `state.actor::<ForgeActor>()` from `AppState`.
    /// Authentication is handled by the upstream acton-service token middleware
    /// which injects `Claims` into extensions.
    pub fn register_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let forge_router = forge_routes()
            .with_state(AppState::<SchemaForgeConfig>::default());
        router.nest("/forge", forge_router)
    }

    /// Register SchemaForge routes into a VersionedApiBuilder.
    ///
    /// Nests forge routes (schemas + entities CRUD) under `/forge` within the
    /// specified API version. Route handlers access the `ForgeActor` via
    /// `state.actor::<ForgeActor>()` from `AppState`.
    ///
    /// ```rust,ignore
    /// let routes = VersionedApiBuilder::new()
    ///     .with_base_path("/api")
    ///     .add_version(ApiVersion::V1, |router| {
    ///         extension.register_versioned_routes(router)
    ///     })
    ///     .build_routes();
    /// ```
    pub fn register_versioned_routes<T>(
        &self,
        router: Router<acton_service::state::AppState<T>>,
    ) -> Router<acton_service::state::AppState<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Clone + Default + Send + Sync + 'static,
    {
        let forge_router: Router<()> = forge_routes()
            .with_state(AppState::<SchemaForgeConfig>::default());
        router.nest_service("/forge", forge_router)
    }

    /// Register versioned forge routes without requiring a `SchemaForgeExtension` instance.
    ///
    /// This is a standalone function for use with the actor-based flow where no
    /// `SchemaForgeExtension` instance is needed — the `ForgeActor` provides state
    /// and the routes are stateless with respect to `ForgeState`.
    pub fn versioned_forge_routes(
        router: Router<AppState<SchemaForgeConfig>>,
    ) -> Router<AppState<SchemaForgeConfig>> {
        router.nest("/forge", forge_routes())
    }

    /// Register admin UI routes onto an existing Router.
    ///
    /// Nests admin routes under `/admin/`, with a redirect from `/admin` to `/admin/`
    /// so both URL forms work correctly in browsers.
    ///
    /// Applies an in-memory session layer for session-based authentication.
    /// Protected routes require authentication; unauthenticated requests are
    /// redirected to `/admin/login`.
    pub fn register_admin_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        use axum::response::Redirect;
        use axum::routing::get;

        let admin_router = crate::admin::routes::admin_routes()
            .layer(self.session_layer.clone())
            .with_state(self.state.clone());
        router
            .nest("/admin/", admin_router)
            .route("/admin", get(|| async { Redirect::permanent("/admin/") }))
    }

    /// Register GraphQL routes onto an existing Router.
    ///
    /// Only available when the `graphql` feature is enabled.
    /// Adds `POST /forge/graphql` (handler) and `GET /forge/graphql` (GraphiQL playground).
    /// Claims are extracted from request extensions (injected by upstream token middleware).
    #[cfg(feature = "graphql")]
    pub fn register_graphql_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let gql_router = Router::new()
            .route(
                "/graphql",
                axum::routing::get(crate::graphql::graphql_playground)
                    .post(crate::graphql::graphql_handler),
            )
            .with_state(self.state.clone());
        router.nest("/forge", gql_router)
    }

    /// Register widget routes onto an existing Router.
    ///
    /// Nests widget routes under `/forge/`, serving bare HTMX fragments
    /// for entity CRUD operations that can be embedded in any HTMX application.
    ///
    /// Claims are extracted from request extensions so widget requests respect
    /// schema `@access` annotations and field-level filtering.
    pub fn register_widget_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let widget_router = crate::widget::routes::widget_routes()
            .layer(self.session_layer.clone())
            .with_state(self.state.clone());
        router.nest("/forge", widget_router)
    }

    /// Build admin UI routes as a standalone `Router<()>` (state pre-applied).
    ///
    /// Use this to nest admin routes into an `AppState`-based router
    /// (e.g. via `VersionedApiBuilder::with_frontend_routes`). The session
    /// layer is **not** included — apply it externally so admin and widget
    /// routes share the same layer.
    pub fn admin_frontend_router(&self) -> axum::Router {
        crate::admin::routes::admin_routes().with_state(self.state.clone())
    }

    /// Build site UI routes as a standalone `Router<()>` (state pre-applied).
    ///
    /// Includes login/logout and a home page listing available schemas.
    /// The session layer is **not** included — apply it externally so admin,
    /// widget, and site routes share the same layer.
    pub fn site_frontend_router(&self) -> axum::Router {
        crate::site::routes::site_routes().with_state(self.state.clone())
    }

    /// Build widget UI routes as a standalone `Router<()>` (state pre-applied).
    ///
    /// Includes the `session_to_claims` middleware so browser sessions are
    /// automatically bridged to `Claims` in request extensions. The session
    /// layer is **not** included — apply it externally.
    pub fn widget_frontend_router(&self) -> axum::Router {
        crate::widget::routes::widget_routes().with_state(self.state.clone())
    }

    /// Get a clone of the shared session layer.
    ///
    /// Apply this to a router that contains both admin and widget routes
    /// so they share a single in-memory session store.
    pub fn session_layer(&self) -> SessionManagerLayer<MemoryStore> {
        self.session_layer.clone()
    }

    /// Get a reference to the schema registry.
    pub fn registry(&self) -> &SchemaRegistry {
        &self.state.registry
    }

    /// Get a reference to the `ForgeState`.
    ///
    /// Useful for direct access to the state when building custom routes.
    pub fn state(&self) -> &ForgeState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_without_backend_produces_error() {
        let result = SchemaForgeExtensionBuilder::new().build().await;
        assert!(result.is_err());
        if let Err(ForgeError::Internal { message }) = result {
            assert!(message.contains("backend"));
        } else {
            panic!("expected ForgeError::Internal");
        }
    }

    // Note: Full integration tests with a real backend are in tests/integration.rs
}
