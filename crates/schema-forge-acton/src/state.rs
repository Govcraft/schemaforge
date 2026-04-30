use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::traits::{EntityStore, SchemaBackend};
use schema_forge_backend::user_store::{AuthStore, ForgeUser};
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};
use sync_wrapper::SyncFuture;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// DynSchemaBackend
// ---------------------------------------------------------------------------

/// Object-safe wrapper for `SchemaBackend`.
///
/// RPITIT traits cannot be used as `dyn Trait`. This wrapper uses boxed futures
/// to enable dynamic dispatch for HTTP handler state.
pub trait DynSchemaBackend: Send + Sync {
    /// Apply a sequence of migration steps to a schema table.
    fn apply_migration<'a>(
        &'a self,
        schema_name: &'a SchemaName,
        steps: &'a [MigrationStep],
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>>;

    /// Store (upsert) schema metadata in the backend.
    fn store_schema_metadata<'a>(
        &'a self,
        definition: &'a SchemaDefinition,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>>;

    /// Load schema metadata by name.
    fn load_schema_metadata<'a>(
        &'a self,
        name: &'a SchemaName,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SchemaDefinition>, BackendError>> + Send + Sync + 'a>,
    >;

    /// List all stored schema metadata.
    fn list_schema_metadata(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SchemaDefinition>, BackendError>> + Send + Sync + '_>>;
}

/// Blanket impl: any concrete `SchemaBackend` automatically implements `DynSchemaBackend`.
impl<T: SchemaBackend + 'static> DynSchemaBackend for T {
    fn apply_migration<'a>(
        &'a self,
        schema_name: &'a SchemaName,
        steps: &'a [MigrationStep],
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(SchemaBackend::apply_migration(
            self,
            schema_name,
            steps,
        )))
    }

    fn store_schema_metadata<'a>(
        &'a self,
        definition: &'a SchemaDefinition,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(SchemaBackend::store_schema_metadata(
            self, definition,
        )))
    }

    fn load_schema_metadata<'a>(
        &'a self,
        name: &'a SchemaName,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SchemaDefinition>, BackendError>> + Send + Sync + 'a>,
    > {
        Box::pin(SyncFuture::new(SchemaBackend::load_schema_metadata(
            self, name,
        )))
    }

    fn list_schema_metadata(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SchemaDefinition>, BackendError>> + Send + Sync + '_>>
    {
        Box::pin(SyncFuture::new(SchemaBackend::list_schema_metadata(self)))
    }
}

// ---------------------------------------------------------------------------
// DynEntityStore
// ---------------------------------------------------------------------------

/// Object-safe wrapper for `EntityStore`.
///
/// Same pattern as `DynSchemaBackend`: boxed futures for dynamic dispatch.
pub trait DynEntityStore: Send + Sync {
    /// Create a new entity in the backend.
    ///
    /// The returned future is `Send + Sync` so it can be awaited inside an
    /// acton-reactive `act_on` handler body without going through an inner
    /// `tokio::spawn` (which would orphan the work from the runtime).
    fn create<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + Sync + 'a>>;

    /// Retrieve an entity by schema name and entity ID.
    fn get<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + Sync + 'a>>;

    /// Update an existing entity.
    fn update<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + Sync + 'a>>;

    /// Delete an entity by schema name and entity ID.
    fn delete<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>>;

    /// Execute a query and return matching entities.
    fn query<'a>(
        &'a self,
        query: &'a Query,
    ) -> Pin<Box<dyn Future<Output = Result<QueryResult, BackendError>> + Send + Sync + 'a>>;

    /// Count entities matching a query (ignoring limit/offset).
    fn count<'a>(
        &'a self,
        query: &'a Query,
    ) -> Pin<Box<dyn Future<Output = Result<usize, BackendError>> + Send + Sync + 'a>>;

    /// Compute aggregate values over entities matching a query.
    fn aggregate<'a>(
        &'a self,
        query: &'a AggregateQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<AggregateResult>, BackendError>> + Send + Sync + 'a>>;
}

/// Blanket impl: any concrete `EntityStore` automatically implements `DynEntityStore`.
///
/// The underlying `EntityStore` futures are `Send` only — many backend
/// implementations (notably sqlx-based ones) hold `!Sync` state across
/// `await` points. Wrapping them in [`SyncFuture`] is sound because
/// [`Future::poll`] takes `Pin<&mut Self>` (exclusive access), so no two
/// threads can ever observe the inner non-`Sync` state simultaneously.
/// This tightening lets the boxed future satisfy acton-reactive's
/// `Send + Sync` `FutureBox` bound, so backend calls can be awaited
/// directly inside `act_on` handlers without an inner `tokio::spawn`.
impl<T: EntityStore + 'static> DynEntityStore for T {
    fn create<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(EntityStore::create(self, entity)))
    }

    fn get<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(EntityStore::get(self, schema, id)))
    }

    fn update<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(EntityStore::update(self, entity)))
    }

    fn delete<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(EntityStore::delete(self, schema, id)))
    }

    fn query<'a>(
        &'a self,
        query: &'a Query,
    ) -> Pin<Box<dyn Future<Output = Result<QueryResult, BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(EntityStore::query(self, query)))
    }

    fn count<'a>(
        &'a self,
        query: &'a Query,
    ) -> Pin<Box<dyn Future<Output = Result<usize, BackendError>> + Send + Sync + 'a>> {
        Box::pin(SyncFuture::new(EntityStore::count(self, query)))
    }

    fn aggregate<'a>(
        &'a self,
        query: &'a AggregateQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<AggregateResult>, BackendError>> + Send + Sync + 'a>>
    {
        Box::pin(SyncFuture::new(EntityStore::aggregate(self, query)))
    }
}

// ---------------------------------------------------------------------------
// DynForgeBackend
// ---------------------------------------------------------------------------

/// Combined object-safe trait for backends that implement both
/// `SchemaBackend` and `EntityStore`.
pub trait DynForgeBackend: DynSchemaBackend + DynEntityStore {}

/// Blanket impl for any type that implements both `DynSchemaBackend` and `DynEntityStore`.
impl<T: DynSchemaBackend + DynEntityStore> DynForgeBackend for T {}

// ---------------------------------------------------------------------------
// DynAuthStore
// ---------------------------------------------------------------------------

/// Object-safe wrapper for `AuthStore`.
///
/// Same pattern as `DynSchemaBackend`/`DynEntityStore`: boxed futures for dynamic dispatch.
pub trait DynAuthStore: Send + Sync {
    /// Validate username/password credentials.
    fn validate_credentials<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ForgeUser>, BackendError>> + Send + 'a>>;

    /// List all users ordered by username.
    fn list_users(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ForgeUser>, BackendError>> + Send + '_>>;

    /// Get a single user by username.
    fn get_user<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ForgeUser>, BackendError>> + Send + 'a>>;

    /// Get the raw User entity row by username, including operator-defined
    /// columns (used by the IN-side principal-claim projection at login time).
    fn get_user_entity<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Entity>, BackendError>> + Send + 'a>>;

    /// Create a new user with a plaintext password (will be hashed by the implementation).
    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        roles: &'a [String],
        display_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Update a user's roles and display name.
    fn update_user<'a>(
        &'a self,
        username: &'a str,
        roles: &'a [String],
        display_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Toggle a user's active status.
    fn toggle_user_active<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Count the total number of users.
    fn count_users(&self)
        -> Pin<Box<dyn Future<Output = Result<usize, BackendError>> + Send + '_>>;

    /// Delete a user by username. Idempotent.
    fn delete_user<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Replace a user's password (plaintext; implementation hashes it).
    fn change_password<'a>(
        &'a self,
        username: &'a str,
        new_password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;
}

/// Blanket impl: any concrete `AuthStore` automatically implements `DynAuthStore`.
impl<T: AuthStore + 'static> DynAuthStore for T {
    fn validate_credentials<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ForgeUser>, BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::validate_credentials(self, username, password))
    }

    fn list_users(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ForgeUser>, BackendError>> + Send + '_>> {
        Box::pin(AuthStore::list_users(self))
    }

    fn get_user<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ForgeUser>, BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::get_user(self, username))
    }

    fn get_user_entity<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Entity>, BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::get_user_entity(self, username))
    }

    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        roles: &'a [String],
        display_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::create_user(
            self,
            username,
            password,
            roles,
            display_name,
        ))
    }

    fn update_user<'a>(
        &'a self,
        username: &'a str,
        roles: &'a [String],
        display_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::update_user(self, username, roles, display_name))
    }

    fn toggle_user_active<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::toggle_user_active(self, username))
    }

    fn count_users(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<usize, BackendError>> + Send + '_>> {
        Box::pin(AuthStore::count_users(self))
    }

    fn delete_user<'a>(
        &'a self,
        username: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::delete_user(self, username))
    }

    fn change_password<'a>(
        &'a self,
        username: &'a str,
        new_password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(AuthStore::change_password(self, username, new_password))
    }
}

// ---------------------------------------------------------------------------
// SchemaRegistry
// ---------------------------------------------------------------------------

/// Thread-safe, in-memory cache of registered schema definitions.
///
/// Backed by `Arc<RwLock<HashMap<String, SchemaDefinition>>>`.
/// All mutations go through the backend first, then update the cache.
#[derive(Clone)]
pub struct SchemaRegistry {
    inner: Arc<RwLock<HashMap<String, SchemaDefinition>>>,
}

impl SchemaRegistry {
    /// Creates a new, empty `SchemaRegistry`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a schema definition by name.
    pub async fn get(&self, name: &str) -> Option<SchemaDefinition> {
        let guard = self.inner.read().await;
        guard.get(name).cloned()
    }

    /// List all schema definitions in the registry.
    pub async fn list(&self) -> Vec<SchemaDefinition> {
        let guard = self.inner.read().await;
        guard.values().cloned().collect()
    }

    /// Insert or update a schema definition in the cache.
    pub async fn insert(&self, name: String, definition: SchemaDefinition) {
        let mut guard = self.inner.write().await;
        guard.insert(name, definition);
    }

    /// Remove a schema definition from the cache.
    pub async fn remove(&self, name: &str) -> Option<SchemaDefinition> {
        let mut guard = self.inner.write().await;
        guard.remove(name)
    }

    /// Load all existing schemas from the backend into the cache.
    pub async fn load_from_backend(
        &self,
        backend: &dyn DynSchemaBackend,
    ) -> Result<(), BackendError> {
        let schemas = backend.list_schema_metadata().await?;
        let mut guard = self.inner.write().await;
        guard.clear();
        for schema in schemas {
            let name = schema.name.as_str().to_string();
            guard.insert(name, schema);
        }
        Ok(())
    }
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ForgeState
// ---------------------------------------------------------------------------

/// Shared state for SchemaForge route handlers.
///
/// Stored as an axum `Extension` and accessed via `State` in handlers.
#[derive(Clone)]
pub struct ForgeState {
    /// In-memory cache of registered schema definitions.
    pub registry: SchemaRegistry,
    /// Dynamic dispatch backend for schema and entity operations.
    pub backend: Arc<dyn DynForgeBackend>,
    /// Tenant configuration derived from `@tenant` annotations.
    /// `None` when multi-tenancy is not configured.
    pub tenant_config: Option<schema_forge_backend::tenant::TenantConfig>,
    /// Optional record-level access policy.
    /// When `Some`, entity handlers check ownership before allowing modify/delete.
    pub record_access_policy: Option<Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>>,
    /// Compiled Cedar policy bundle. The single source of truth for every
    /// authorization decision the server makes.
    pub policy_store: Arc<crate::authz::PolicyStore>,
    /// Dynamic GraphQL schema, atomically swappable on schema changes.
    #[cfg(feature = "graphql")]
    pub graphql_schema: Arc<arc_swap::ArcSwap<async_graphql::dynamic::Schema>>,
    /// Auth store for user authentication and management.
    pub auth_store: Option<Arc<dyn DynAuthStore>>,
    /// Optional webhook dispatcher for outbound event notifications.
    pub webhook_dispatcher: Option<Arc<crate::webhook::WebhookDispatcher>>,
    /// Registry of S3-compatible storage backends bound to their `bucket:` name.
    /// Empty when no `[schema_forge.storage]` config is provided.
    pub storage_registry: crate::storage::StorageRegistry,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_registry_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SchemaRegistry>();
    }

    #[test]
    fn forge_state_is_clone_send_sync() {
        fn assert_clone_send_sync<T: Clone + Send + Sync>() {}
        assert_clone_send_sync::<ForgeState>();
    }

    #[tokio::test]
    async fn registry_insert_and_get() {
        let registry = SchemaRegistry::new();
        let schema = make_test_schema("Contact");
        registry.insert("Contact".to_string(), schema.clone()).await;
        let retrieved = registry.get("Contact").await;
        assert_eq!(retrieved, Some(schema));
    }

    #[tokio::test]
    async fn registry_list() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Contact".to_string(), make_test_schema("Contact"))
            .await;
        registry
            .insert("Company".to_string(), make_test_schema("Company"))
            .await;
        let list = registry.list().await;
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn registry_remove() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Contact".to_string(), make_test_schema("Contact"))
            .await;
        let removed = registry.remove("Contact").await;
        assert!(removed.is_some());
        assert!(registry.get("Contact").await.is_none());
    }

    #[tokio::test]
    async fn registry_get_missing_returns_none() {
        let registry = SchemaRegistry::new();
        assert!(registry.get("Missing").await.is_none());
    }

    #[tokio::test]
    async fn registry_remove_missing_returns_none() {
        let registry = SchemaRegistry::new();
        assert!(registry.remove("Missing").await.is_none());
    }

    fn make_test_schema(name: &str) -> SchemaDefinition {
        use schema_forge_core::types::{
            FieldDefinition, FieldName, FieldType, SchemaId, TextConstraints,
        };
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }
}
