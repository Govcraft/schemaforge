//! Message types for the `ForgeActor`.
//!
//! Each message type corresponds to an operation the actor can perform.
//! Messages require `Clone + Debug` (minimum for acton-reactive's `ActonMessage`).
//!
//! Response types are also defined here. Handlers send responses via
//! `reply_envelope().send(response).await` for request-response patterns.

use std::sync::Arc;

use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};

// ---------------------------------------------------------------------------
// Registry reads (sync handlers)
// ---------------------------------------------------------------------------

/// Retrieve a single schema definition by name.
#[derive(Clone, Debug)]
pub struct GetSchema {
    pub name: String,
}

/// Response for [`GetSchema`].
#[derive(Clone, Debug)]
pub struct GetSchemaResponse(pub Option<SchemaDefinition>);

/// List all registered schema definitions.
#[derive(Clone, Debug)]
pub struct ListSchemas;

/// Response for [`ListSchemas`].
#[derive(Clone, Debug)]
pub struct ListSchemasResponse(pub Vec<SchemaDefinition>);

/// Retrieve the current tenant configuration.
#[derive(Clone, Debug)]
pub struct GetTenantConfig;

/// Response for [`GetTenantConfig`].
#[derive(Clone, Debug)]
pub struct GetTenantConfigResponse(pub Option<TenantConfig>);

/// Retrieve the current record access policy.
#[derive(Clone, Debug)]
pub struct GetRecordAccessPolicy;

/// Response for [`GetRecordAccessPolicy`].
///
/// Uses `Arc<dyn RecordAccessPolicy>` because the policy trait object is
/// not `Clone`; wrapping in `Arc` satisfies the `Clone` bound on messages.
/// Manual `Debug` impl because `dyn RecordAccessPolicy` is not `Debug`.
#[derive(Clone)]
pub struct GetRecordAccessPolicyResponse(pub Option<Arc<dyn RecordAccessPolicy>>);

impl std::fmt::Debug for GetRecordAccessPolicyResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("GetRecordAccessPolicyResponse")
            .field(&self.0.as_ref().map(|_| ".."))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Registry mutations (async handlers)
// ---------------------------------------------------------------------------

/// Insert or update a schema definition in the in-memory registry.
#[derive(Clone, Debug)]
pub struct InsertSchema {
    pub name: String,
    pub definition: SchemaDefinition,
}

/// Remove a schema definition from the in-memory registry.
#[derive(Clone, Debug)]
pub struct RemoveSchema {
    pub name: String,
}

/// Response for [`RemoveSchema`].
#[derive(Clone, Debug)]
pub struct RemoveSchemaResponse(pub Option<SchemaDefinition>);

/// Update the tenant configuration.
#[derive(Clone, Debug)]
pub struct UpdateTenantConfig {
    pub config: Option<TenantConfig>,
}

// ---------------------------------------------------------------------------
// Backend operations (async handlers — supervised)
// ---------------------------------------------------------------------------

/// Create a new entity in the backend.
#[derive(Clone, Debug)]
pub struct CreateEntity {
    pub entity: Entity,
}

/// Response for [`CreateEntity`].
#[derive(Clone, Debug)]
pub struct CreateEntityResponse(pub Result<Entity, BackendError>);

/// Retrieve an entity by schema name and entity ID.
#[derive(Clone, Debug)]
pub struct GetEntity {
    pub schema: SchemaName,
    pub id: EntityId,
}

/// Response for [`GetEntity`].
#[derive(Clone, Debug)]
pub struct GetEntityResponse(pub Result<Entity, BackendError>);

/// Update an existing entity.
#[derive(Clone, Debug)]
pub struct UpdateEntity {
    pub entity: Entity,
}

/// Response for [`UpdateEntity`].
#[derive(Clone, Debug)]
pub struct UpdateEntityResponse(pub Result<Entity, BackendError>);

/// Delete an entity by schema name and entity ID.
#[derive(Clone, Debug)]
pub struct DeleteEntity {
    pub schema: SchemaName,
    pub id: EntityId,
}

/// Response for [`DeleteEntity`].
#[derive(Clone, Debug)]
pub struct DeleteEntityResponse(pub Result<(), BackendError>);

/// Execute a query and return matching entities.
#[derive(Clone, Debug)]
pub struct QueryEntities {
    pub query: Query,
}

/// Response for [`QueryEntities`].
#[derive(Clone, Debug)]
pub struct QueryEntitiesResponse(pub Result<QueryResult, BackendError>);

/// Count entities matching a query.
#[derive(Clone, Debug)]
pub struct CountEntities {
    pub query: Query,
}

/// Response for [`CountEntities`].
#[derive(Clone, Debug)]
pub struct CountEntitiesResponse(pub Result<usize, BackendError>);

/// Compute aggregate values over entities matching a query.
#[derive(Clone, Debug)]
pub struct AggregateEntities {
    pub query: AggregateQuery,
}

/// Response for [`AggregateEntities`].
#[derive(Clone, Debug)]
pub struct AggregateEntitiesResponse(pub Result<Vec<AggregateResult>, BackendError>);

/// Apply migration steps to a schema table.
#[derive(Clone, Debug)]
pub struct ApplyMigration {
    pub schema_name: SchemaName,
    pub steps: Vec<MigrationStep>,
}

/// Response for [`ApplyMigration`].
#[derive(Clone, Debug)]
pub struct ApplyMigrationResponse(pub Result<(), BackendError>);

/// Store (upsert) schema metadata in the backend.
#[derive(Clone, Debug)]
pub struct StoreSchemaMetadata {
    pub definition: SchemaDefinition,
}

/// Response for [`StoreSchemaMetadata`].
#[derive(Clone, Debug)]
pub struct StoreSchemaMetadataResponse(pub Result<(), BackendError>);

/// Load schema metadata from the backend by name.
#[derive(Clone, Debug)]
pub struct LoadSchemaMetadata {
    pub name: SchemaName,
}

/// Response for [`LoadSchemaMetadata`].
#[derive(Clone, Debug)]
pub struct LoadSchemaMetadataResponse(pub Result<Option<SchemaDefinition>, BackendError>);
