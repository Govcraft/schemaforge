use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;

use crate::error::ForgeError;
use crate::routes::{auth_routes, forge_routes};
use crate::state::{
    DynAuthStore, DynEntityStore, DynForgeBackend, DynSchemaBackend, ForgeState, SchemaRegistry,
};

use crate::config::SchemaForgeConfig;
use crate::storage::{StorageConfig, StorageRegistry};
use acton_service::state::AppState;
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
    /// Optional hook dispatcher for `@hook` lifecycle events.
    pub hook_dispatcher: Option<Arc<dyn crate::hooks::HookDispatcher>>,
    /// Registry of S3-compatible storage backends for `file` fields.
    /// Empty when no `[schema_forge.storage]` config is provided.
    pub storage_registry: StorageRegistry,
    /// Compiled Cedar policy bundle. The single source of truth for every
    /// authorization decision the actor will make once initialized.
    pub policy_store: Option<Arc<crate::authz::PolicyStore>>,
}

// ---------------------------------------------------------------------------
// SchemaForgeExtension
// ---------------------------------------------------------------------------

/// Builder for SchemaForge's acton-service integration.
///
/// The extension is now only responsible for bootstrapping the shared
/// [`ForgeState`] and (optionally) seeding an initial admin user into the
/// auth store. All UI surfaces are generated client-side by the
/// `schemaforge site generate` command; this crate only hosts the JSON API.
pub struct SchemaForgeExtension {
    state: ForgeState,
}

/// Builder for `SchemaForgeExtension`.
pub struct SchemaForgeExtensionBuilder {
    backend: Option<Arc<dyn DynForgeBackend>>,
    record_access_policy: Option<Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>>,
    auth_store: Option<Arc<dyn DynAuthStore>>,
    admin_credentials: Option<(String, String)>,
    webhook_config: crate::webhook::WebhookConfig,
    storage_config: StorageConfig,
    role_ranks: crate::authz::role_ranks::RoleRanks,
    principal_claims: crate::authz::principal_claims::PrincipalClaimMappings,
}

impl SchemaForgeExtensionBuilder {
    /// Create a new builder.
    fn new() -> Self {
        Self {
            backend: None,
            record_access_policy: None,
            auth_store: None,
            admin_credentials: None,
            webhook_config: crate::webhook::WebhookConfig::default(),
            storage_config: StorageConfig::default(),
            role_ranks: crate::authz::role_ranks::RoleRanks::empty(),
            principal_claims: crate::authz::principal_claims::PrincipalClaimMappings::default(),
        }
    }

    /// Set the operator-defined PASETO custom-claim → `Forge::Principal`
    /// attribute mappings used by hand-written Cedar policies. Defaults to
    /// the empty mapping (no operator-supplied attributes); embedders
    /// typically construct one via
    /// [`PrincipalClaimMappings::from_config`](crate::authz::principal_claims::PrincipalClaimMappings::from_config)
    /// from `[schema_forge.authz.principal_claims]`.
    pub fn with_principal_claims(
        mut self,
        mappings: crate::authz::principal_claims::PrincipalClaimMappings,
    ) -> Self {
        self.principal_claims = mappings;
        self
    }

    /// Set the role-name → numeric-rank hierarchy used by the Cedar
    /// no-upward-visibility guard. Defaults to the empty hierarchy
    /// (`platform_admin` only). Embedders typically load this from
    /// `policies/role_ranks.toml` via `RoleRanks::from_toml_file`.
    pub fn with_role_ranks(mut self, ranks: crate::authz::role_ranks::RoleRanks) -> Self {
        self.role_ranks = ranks;
        self
    }

    /// Set the S3-compatible storage configuration for `file` fields.
    pub fn with_storage_config(mut self, config: StorageConfig) -> Self {
        self.storage_config = config;
        self
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

        // Initialize webhook dispatcher if enabled
        let webhook_dispatcher = if self.webhook_config.enabled {
            tracing::info!("webhook system enabled");
            Some(Arc::new(crate::webhook::WebhookDispatcher::new(
                self.webhook_config,
            )))
        } else {
            None
        };

        // Initialize storage registry (empty if no backends configured).
        let storage_registry = StorageRegistry::from_config(&self.storage_config)
            .await
            .map_err(|e| ForgeError::Internal {
                message: format!("Failed to initialize storage registry: {e}"),
            })?;
        if storage_registry.is_enabled() {
            tracing::info!(
                backends = storage_registry.len(),
                "storage registry initialized"
            );
        }

        // Validate every `file(bucket: ...)` in the schema registry references a
        // configured backend. Fail loud at startup rather than at first request.
        validate_file_references(&all_schemas, &storage_registry)?;

        // Compile the Cedar policy bundle from the registered schemas. This is
        // the single source of truth for every authorization decision the
        // server will make. Invalid or non-validating bundles fail startup —
        // partial installs are not acceptable for a gov-audit posture.
        let policy_store = crate::authz::PolicyStore::new(
            crate::authz::store::PolicyStoreSnapshot::from_schemas(
                &all_schemas,
                None,
                self.role_ranks,
                self.principal_claims,
            )
            .map_err(|e| ForgeError::Internal {
                message: format!("Cedar policy compilation failed at startup: {e}"),
            })?,
        );
        let policy_store = Arc::new(policy_store);

        // Default record-access policy: Cedar engine on the same store. An
        // operator-supplied custom policy passed to the builder takes
        // precedence; this fallback ensures every deployment has a
        // record-level enforcement path even without explicit wiring.
        let record_access_policy: Option<Arc<dyn RecordAccessPolicy>> =
            self.record_access_policy.or_else(|| {
                Some(Arc::new(crate::authz::CedarRecordPolicy::new(
                    policy_store.clone(),
                )))
            });

        let state = ForgeState {
            registry,
            backend,
            tenant_config,
            record_access_policy,
            policy_store,
            #[cfg(feature = "graphql")]
            graphql_schema,
            auth_store: self.auth_store,
            webhook_dispatcher,
            storage_registry,
        };

        Ok(SchemaForgeExtension { state })
    }
}

/// Fail startup if any schema has a `file` field pointing at a bucket name that
/// is not declared in `[schema_forge.storage.backends]`. File uploads require a
/// real backend, so this is a misconfiguration we refuse to run under.
fn validate_file_references(
    schemas: &[SchemaDefinition],
    storage: &StorageRegistry,
) -> Result<(), ForgeError> {
    use schema_forge_core::types::FieldType;

    let mut missing: Vec<String> = Vec::new();
    for schema in schemas {
        for field in &schema.fields {
            if let FieldType::File(constraints) = &field.field_type {
                if storage.get(&constraints.bucket).is_none() {
                    missing.push(format!(
                        "{}::{} -> bucket \"{}\"",
                        schema.name.as_str(),
                        field.name.as_str(),
                        constraints.bucket
                    ));
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ForgeError::Internal {
            message: format!(
                "file field(s) reference undeclared storage backends: [{}]. \
                 Add matching [schema_forge.storage.backends.<name>] entries.",
                missing.join(", ")
            ),
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
        storage_config: &StorageConfig,
        role_ranks: crate::authz::role_ranks::RoleRanks,
        principal_claims: crate::authz::principal_claims::PrincipalClaimMappings,
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

        // Run the inverse-relation pairing pass now that we have every
        // schema visible in one batch. Stored metadata from pre-#34 DBs
        // won't have `derived_from` set, so this pass recomputes it on
        // every daemon start — cheap and idempotent.
        let mut paired: Vec<SchemaDefinition> = registry.values().cloned().collect();
        schema_forge_core::inverse_relations::pair_inverse_relations(&mut paired).map_err(
            |e| ForgeError::Internal {
                message: format!("invalid inverse relation: {e}"),
            },
        )?;
        for schema in paired {
            registry.insert(schema.name.as_str().to_string(), schema);
        }

        // Build tenant config from all registered schemas
        let all_schemas: Vec<SchemaDefinition> = registry.values().cloned().collect();
        let tenant_config =
            TenantConfig::from_schemas(&all_schemas).map_err(|e| ForgeError::Internal {
                message: format!("Invalid tenant configuration: {e}"),
            })?;
        let tenant_config = if tenant_config.is_enabled() {
            Some(tenant_config)
        } else {
            None
        };

        // Initialize the S3 storage registry (empty if no backends configured).
        let storage_registry =
            StorageRegistry::from_config(storage_config)
                .await
                .map_err(|e| ForgeError::Internal {
                    message: format!("Failed to initialize storage registry: {e}"),
                })?;
        validate_file_references(&all_schemas, &storage_registry)?;
        if storage_registry.is_enabled() {
            tracing::info!(
                backends = storage_registry.len(),
                "storage registry initialized"
            );
        }

        // Compile the Cedar policy bundle from the registered schemas. Same
        // contract as the standalone build path: validation failures abort
        // initialization rather than producing a partial install.
        let policy_store = crate::authz::PolicyStore::new(
            crate::authz::store::PolicyStoreSnapshot::from_schemas(
                &all_schemas,
                None,
                role_ranks,
                principal_claims,
            )
            .map_err(|e| ForgeError::Internal {
                message: format!("Cedar policy compilation failed at startup: {e}"),
            })?,
        );
        let policy_store = Some(Arc::new(policy_store));

        Ok(InitForgeData {
            registry,
            backend,
            tenant_config,
            record_access_policy,
            hook_dispatcher: None,
            storage_registry,
            policy_store,
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
        let forge_router = forge_routes().with_state(AppState::<SchemaForgeConfig>::default());
        router.nest("/forge", forge_router)
    }

    /// Register SchemaForge routes into a VersionedApiBuilder.
    ///
    /// Nests forge routes (schemas + entities CRUD) under `/forge` within the
    /// specified API version. Route handlers access the `ForgeActor` via
    /// `state.actor::<ForgeActor>()` from `AppState`.
    pub fn register_versioned_routes<T>(
        &self,
        router: Router<acton_service::state::AppState<T>>,
    ) -> Router<acton_service::state::AppState<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Clone + Default + Send + Sync + 'static,
    {
        let forge_router: Router<()> =
            forge_routes().with_state(AppState::<SchemaForgeConfig>::default());
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
        router.nest(
            "/forge",
            forge_routes()
                .merge(auth_routes())
                .merge(crate::routes::meta_routes()),
        )
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
