use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::traits::{EntityStore, SchemaBackend};
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::Query;
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};
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
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Store (upsert) schema metadata in the backend.
    fn store_schema_metadata<'a>(
        &'a self,
        definition: &'a SchemaDefinition,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Load schema metadata by name.
    fn load_schema_metadata<'a>(
        &'a self,
        name: &'a SchemaName,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SchemaDefinition>, BackendError>> + Send + 'a>>;

    /// List all stored schema metadata.
    fn list_schema_metadata(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SchemaDefinition>, BackendError>> + Send + '_>>;
}

/// Blanket impl: any concrete `SchemaBackend` automatically implements `DynSchemaBackend`.
impl<T: SchemaBackend + 'static> DynSchemaBackend for T {
    fn apply_migration<'a>(
        &'a self,
        schema_name: &'a SchemaName,
        steps: &'a [MigrationStep],
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(SchemaBackend::apply_migration(self, schema_name, steps))
    }

    fn store_schema_metadata<'a>(
        &'a self,
        definition: &'a SchemaDefinition,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(SchemaBackend::store_schema_metadata(self, definition))
    }

    fn load_schema_metadata<'a>(
        &'a self,
        name: &'a SchemaName,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SchemaDefinition>, BackendError>> + Send + 'a>>
    {
        Box::pin(SchemaBackend::load_schema_metadata(self, name))
    }

    fn list_schema_metadata(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SchemaDefinition>, BackendError>> + Send + '_>>
    {
        Box::pin(SchemaBackend::list_schema_metadata(self))
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
    fn create<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + 'a>>;

    /// Retrieve an entity by schema name and entity ID.
    fn get<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + 'a>>;

    /// Update an existing entity.
    fn update<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + 'a>>;

    /// Delete an entity by schema name and entity ID.
    fn delete<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>>;

    /// Execute a query and return matching entities.
    fn query<'a>(
        &'a self,
        query: &'a Query,
    ) -> Pin<Box<dyn Future<Output = Result<QueryResult, BackendError>> + Send + 'a>>;
}

/// Blanket impl: any concrete `EntityStore` automatically implements `DynEntityStore`.
impl<T: EntityStore + 'static> DynEntityStore for T {
    fn create<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + 'a>> {
        Box::pin(EntityStore::create(self, entity))
    }

    fn get<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + 'a>> {
        Box::pin(EntityStore::get(self, schema, id))
    }

    fn update<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = Result<Entity, BackendError>> + Send + 'a>> {
        Box::pin(EntityStore::update(self, entity))
    }

    fn delete<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + 'a>> {
        Box::pin(EntityStore::delete(self, schema, id))
    }

    fn query<'a>(
        &'a self,
        query: &'a Query,
    ) -> Pin<Box<dyn Future<Output = Result<QueryResult, BackendError>> + Send + 'a>> {
        Box::pin(EntityStore::query(self, query))
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
