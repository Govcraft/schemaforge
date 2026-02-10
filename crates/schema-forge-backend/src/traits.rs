use std::future::Future;

use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::Query;
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};

use crate::entity::{Entity, QueryResult};
use crate::error::BackendError;

/// Storage-agnostic trait for schema lifecycle operations.
///
/// Implementations handle:
/// - Applying migration steps (DDL) to the underlying storage
/// - Storing and retrieving schema metadata
///
/// Uses RPITIT (return position impl Trait in trait) for async methods,
/// avoiding the `async-trait` crate.
pub trait SchemaBackend: Send + Sync {
    /// Apply a sequence of migration steps to a schema table.
    ///
    /// Each step is translated to the backend's native DDL and executed.
    /// Steps are applied in order. If any step fails, the error is returned
    /// and no further steps are executed.
    fn apply_migration(
        &self,
        schema_name: &SchemaName,
        steps: &[MigrationStep],
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Store (upsert) schema metadata in the backend.
    ///
    /// This stores the full `SchemaDefinition` so it can be retrieved later
    /// for diffing, validation, or introspection.
    fn store_schema_metadata(
        &self,
        definition: &SchemaDefinition,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Load schema metadata by name.
    ///
    /// Returns `None` if the schema has never been stored.
    fn load_schema_metadata(
        &self,
        name: &SchemaName,
    ) -> impl Future<Output = Result<Option<SchemaDefinition>, BackendError>> + Send;

    /// List all stored schema metadata.
    fn list_schema_metadata(
        &self,
    ) -> impl Future<Output = Result<Vec<SchemaDefinition>, BackendError>> + Send;
}

/// Storage-agnostic trait for entity (record) CRUD operations.
///
/// Implementations handle:
/// - Creating, reading, updating, and deleting entities
/// - Executing queries with filters, sorting, and pagination
pub trait EntityStore: Send + Sync {
    /// Create a new entity in the backend.
    ///
    /// The entity's `id` and `schema` determine where it is stored.
    /// Returns the created entity (which may have backend-generated fields).
    fn create(
        &self,
        entity: &Entity,
    ) -> impl Future<Output = Result<Entity, BackendError>> + Send;

    /// Retrieve an entity by schema name and entity ID.
    ///
    /// Returns `BackendError::EntityNotFound` if the entity does not exist.
    fn get(
        &self,
        schema: &SchemaName,
        id: &EntityId,
    ) -> impl Future<Output = Result<Entity, BackendError>> + Send;

    /// Update an existing entity.
    ///
    /// The entity's `id` and `schema` determine which record to update.
    /// All fields in `entity.fields` replace the existing fields.
    /// Returns the updated entity.
    fn update(
        &self,
        entity: &Entity,
    ) -> impl Future<Output = Result<Entity, BackendError>> + Send;

    /// Delete an entity by schema name and entity ID.
    ///
    /// Returns `BackendError::EntityNotFound` if the entity does not exist.
    fn delete(
        &self,
        schema: &SchemaName,
        id: &EntityId,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Execute a query and return matching entities.
    ///
    /// The query's `schema` field determines the table, and its
    /// filter, sort, limit, and offset clauses are translated to
    /// the backend's native query language.
    fn query(
        &self,
        query: &Query,
    ) -> impl Future<Output = Result<QueryResult, BackendError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time verification that traits have the correct bounds.
    // These functions are never called -- they just verify the trait is object-safe enough
    // for RPITIT usage and that Send + Sync is required.
    fn _assert_schema_backend_send_sync<T: SchemaBackend>() {}
    fn _assert_entity_store_send_sync<T: EntityStore>() {}
}
