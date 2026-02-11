use std::sync::Arc;

use axum::Router;

use crate::error::ForgeError;
use crate::routes::forge_routes;
use crate::state::{DynEntityStore, DynForgeBackend, DynSchemaBackend, ForgeState, SchemaRegistry};

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
}

/// Builder for `SchemaForgeExtension`.
pub struct SchemaForgeExtensionBuilder {
    backend: Option<Arc<dyn DynForgeBackend>>,
    auth_provider: Option<Arc<dyn crate::auth::AuthProvider>>,
    record_access_policy: Option<Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>>,
    #[cfg(feature = "admin-ui")]
    surreal_client: Option<
        schema_forge_surrealdb::surrealdb::Surreal<
            schema_forge_surrealdb::surrealdb::engine::any::Any,
        >,
    >,
    #[cfg(feature = "admin-ui")]
    admin_credentials: Option<(String, String)>,
}

impl SchemaForgeExtensionBuilder {
    /// Create a new builder.
    fn new() -> Self {
        Self {
            backend: None,
            auth_provider: None,
            record_access_policy: None,
            #[cfg(feature = "admin-ui")]
            surreal_client: None,
            #[cfg(feature = "admin-ui")]
            admin_credentials: None,
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

    /// Set the auth provider for API request authentication.
    ///
    /// When configured, the auth middleware will call the provider to
    /// authenticate each request and inject an [`AuthContext`] into
    /// request extensions.
    ///
    /// [`AuthContext`]: schema_forge_backend::auth::AuthContext
    pub fn with_auth_provider<P: crate::auth::AuthProvider + 'static>(
        mut self,
        provider: P,
    ) -> Self {
        self.auth_provider = Some(Arc::new(provider));
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

    /// Set the SurrealDB client for authentication queries.
    ///
    /// The same client used for `SurrealBackend::from_client()` can be cloned
    /// and passed here for auth queries against the `_forge_users` table.
    #[cfg(feature = "admin-ui")]
    pub fn with_surreal_client(
        mut self,
        client: schema_forge_surrealdb::surrealdb::Surreal<
            schema_forge_surrealdb::surrealdb::engine::any::Any,
        >,
    ) -> Self {
        self.surreal_client = Some(client);
        self
    }

    /// Set bootstrap credentials for the initial admin user.
    ///
    /// If the `_forge_users` table is empty during `build()`, an admin user
    /// will be created with these credentials.
    #[cfg(feature = "admin-ui")]
    pub fn with_admin_credentials(mut self, username: String, password: String) -> Self {
        self.admin_credentials = Some((username, password));
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
        #[cfg(feature = "admin-ui")]
        if let (Some(ref db), Some((ref username, ref password))) =
            (&self.surreal_client, &self.admin_credentials)
        {
            crate::admin::auth::bootstrap_admin(db, username, password)
                .await
                .map_err(|e| ForgeError::Internal {
                    message: format!("Admin bootstrap failed: {e}"),
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

        let state = ForgeState {
            registry,
            backend,
            auth_provider: self.auth_provider,
            tenant_config,
            record_access_policy: self.record_access_policy,
            #[cfg(feature = "graphql")]
            graphql_schema,
            #[cfg(feature = "admin-ui")]
            surreal_client: self.surreal_client,
        };

        Ok(SchemaForgeExtension { state })
    }
}

impl SchemaForgeExtension {
    /// Create a new builder for `SchemaForgeExtension`.
    pub fn builder() -> SchemaForgeExtensionBuilder {
        SchemaForgeExtensionBuilder::new()
    }

    /// Register SchemaForge routes onto an existing Router.
    ///
    /// Merges the forge routes (nested under `/forge`) and applies
    /// `ForgeState` as a layer. Auth middleware is applied to all forge routes
    /// when `ForgeState::auth_provider` is configured.
    pub fn register_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let forge_router = forge_routes()
            .route_layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                crate::middleware::auth_middleware,
            ))
            .with_state(self.state.clone());
        router.nest("/forge", forge_router)
    }

    /// Register SchemaForge routes into a VersionedApiBuilder.
    ///
    /// Nests forge routes (schemas + entities CRUD) under `/forge` within the
    /// specified API version. The `ForgeState` is applied internally, so the
    /// returned router is compatible with any `AppState<T>`.
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
            .route_layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                crate::middleware::auth_middleware,
            ))
            .with_state(self.state.clone());
        router.nest_service("/forge", forge_router)
    }

    /// Register admin UI routes onto an existing Router.
    ///
    /// Only available when the `admin-ui` feature is enabled.
    /// Nests admin routes under `/admin/`, with a redirect from `/admin` to `/admin/`
    /// so both URL forms work correctly in browsers.
    ///
    /// Applies an in-memory session layer for session-based authentication.
    /// Protected routes require authentication; unauthenticated requests are
    /// redirected to `/admin/login`.
    #[cfg(feature = "admin-ui")]
    pub fn register_admin_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        use axum::response::Redirect;
        use axum::routing::get;

        let session_config = acton_service::session::SessionConfig {
            secure: false,
            cookie_name: "forge_admin".to_string(),
            ..Default::default()
        };
        let session_layer = acton_service::session::create_memory_session_layer(&session_config);

        let admin_router = crate::admin::routes::admin_routes()
            .layer(session_layer)
            .with_state(self.state.clone());
        router
            .nest("/admin/", admin_router)
            .route("/admin", get(|| async { Redirect::permanent("/admin/") }))
    }

    /// Register GraphQL routes onto an existing Router.
    ///
    /// Only available when the `graphql` feature is enabled.
    /// Adds `POST /forge/graphql` (handler) and `GET /forge/graphql` (GraphiQL playground).
    /// Auth middleware is applied so the GraphQL context gets the authenticated user.
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
            .route_layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                crate::middleware::auth_middleware,
            ))
            .with_state(self.state.clone());
        router.nest("/forge", gql_router)
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
