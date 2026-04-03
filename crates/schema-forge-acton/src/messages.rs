//! Message types for the `ForgeActor`.
//!
//! Each message type corresponds to an operation the actor can perform.
//! Messages require `Clone + Debug + Send + Sync` (for acton-reactive's `ActonMessage`).
//!
//! Request-response messages embed a [`ReplyChannel<T>`] that the actor handler
//! uses to send the response back to the caller via a `tokio::sync::oneshot` channel.
//! Fire-and-forget messages (mutations with no response) omit the reply channel.

use std::sync::Arc;

use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};
use tokio::sync::{oneshot, Mutex};

// ---------------------------------------------------------------------------
// ReplyChannel — Clone-safe oneshot wrapper
// ---------------------------------------------------------------------------

/// A single-use reply channel that satisfies `Clone` (required by `ActonMessage`).
///
/// Wraps a `tokio::sync::oneshot::Sender<T>` in `Arc<Mutex<Option<...>>>` so that
/// `Clone` is trivially implemented (via `Arc::clone`). Only the first call to
/// [`send`](ReplyChannel::send) delivers the value; subsequent calls are no-ops.
pub struct ReplyChannel<T>(Arc<Mutex<Option<oneshot::Sender<T>>>>);

impl<T> ReplyChannel<T> {
    /// Create a new `ReplyChannel` from a `oneshot::Sender`.
    pub fn new(sender: oneshot::Sender<T>) -> Self {
        Self(Arc::new(Mutex::new(Some(sender))))
    }

    /// Send a value through the channel. Only the first call delivers; subsequent
    /// calls are silently ignored (the sender has already been consumed).
    pub async fn send(self, value: T) {
        if let Some(sender) = self.0.lock().await.take() {
            let _ = sender.send(value);
        }
    }
}

impl<T> Clone for ReplyChannel<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::fmt::Debug for ReplyChannel<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ReplyChannel").field(&"..").finish()
    }
}

// ---------------------------------------------------------------------------
// Registry reads (request-response)
// ---------------------------------------------------------------------------

/// Retrieve a single schema definition by name.
#[derive(Clone, Debug)]
pub struct GetSchema {
    pub name: String,
    pub reply: ReplyChannel<Option<SchemaDefinition>>,
}

/// List all registered schema definitions.
#[derive(Clone, Debug)]
pub struct ListSchemas {
    pub reply: ReplyChannel<Vec<SchemaDefinition>>,
}

/// Retrieve the current tenant configuration.
#[derive(Clone, Debug)]
pub struct GetTenantConfig {
    pub reply: ReplyChannel<Option<TenantConfig>>,
}

/// Retrieve the current record access policy.
///
/// Uses `Arc<dyn RecordAccessPolicy>` because the policy trait object is
/// not `Clone`; wrapping in `Arc` satisfies the `Clone` bound on messages.
#[derive(Clone, Debug)]
pub struct GetRecordAccessPolicy {
    pub reply: ReplyChannel<Option<Arc<dyn RecordAccessPolicy>>>,
}

// ---------------------------------------------------------------------------
// Registry mutations
// ---------------------------------------------------------------------------

/// Insert or update a schema definition in the in-memory registry.
/// Fire-and-forget — no reply channel needed.
#[derive(Clone, Debug)]
pub struct InsertSchema {
    pub name: String,
    pub definition: SchemaDefinition,
}

/// Remove a schema definition from the in-memory registry.
/// Returns the removed definition (if any) via the reply channel.
#[derive(Clone, Debug)]
pub struct RemoveSchema {
    pub name: String,
    pub reply: ReplyChannel<Option<SchemaDefinition>>,
}

/// Update the tenant configuration.
/// Fire-and-forget — no reply channel needed.
#[derive(Clone, Debug)]
pub struct UpdateTenantConfig {
    pub config: Option<TenantConfig>,
}

// ---------------------------------------------------------------------------
// Backend operations (request-response via supervised tokio::spawn)
// ---------------------------------------------------------------------------

/// Create a new entity in the backend.
#[derive(Clone, Debug)]
pub struct CreateEntity {
    pub entity: Entity,
    pub reply: ReplyChannel<Result<Entity, BackendError>>,
}

/// Retrieve an entity by schema name and entity ID.
#[derive(Clone, Debug)]
pub struct GetEntity {
    pub schema: SchemaName,
    pub id: EntityId,
    pub reply: ReplyChannel<Result<Entity, BackendError>>,
}

/// Update an existing entity.
#[derive(Clone, Debug)]
pub struct UpdateEntity {
    pub entity: Entity,
    pub reply: ReplyChannel<Result<Entity, BackendError>>,
}

/// Delete an entity by schema name and entity ID.
#[derive(Clone, Debug)]
pub struct DeleteEntity {
    pub schema: SchemaName,
    pub id: EntityId,
    pub reply: ReplyChannel<Result<(), BackendError>>,
}

/// Execute a query and return matching entities.
#[derive(Clone, Debug)]
pub struct QueryEntities {
    pub query: Query,
    pub reply: ReplyChannel<Result<QueryResult, BackendError>>,
}

/// Count entities matching a query.
#[derive(Clone, Debug)]
pub struct CountEntities {
    pub query: Query,
    pub reply: ReplyChannel<Result<usize, BackendError>>,
}

/// Compute aggregate values over entities matching a query.
#[derive(Clone, Debug)]
pub struct AggregateEntities {
    pub query: AggregateQuery,
    pub reply: ReplyChannel<Result<Vec<AggregateResult>, BackendError>>,
}

/// Apply migration steps to a schema table.
#[derive(Clone, Debug)]
pub struct ApplyMigration {
    pub schema_name: SchemaName,
    pub steps: Vec<MigrationStep>,
    pub reply: ReplyChannel<Result<(), BackendError>>,
}

/// Store (upsert) schema metadata in the backend.
#[derive(Clone, Debug)]
pub struct StoreSchemaMetadata {
    pub definition: SchemaDefinition,
    pub reply: ReplyChannel<Result<(), BackendError>>,
}

/// Load schema metadata from the backend by name.
#[derive(Clone, Debug)]
pub struct LoadSchemaMetadata {
    pub name: SchemaName,
    pub reply: ReplyChannel<Result<Option<SchemaDefinition>, BackendError>>,
}
