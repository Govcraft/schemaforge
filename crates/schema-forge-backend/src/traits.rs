use std::future::Future;

use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{DynamicValue, EntityId, FieldName, SchemaDefinition, SchemaName};

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
    /// Atomically apply DDL and persist the resulting metadata, including integrity checks.
    /// `None` removes metadata; physical deletion requires explicit migration steps.
    /// Unsupported adapters must refuse before making any changes.
    fn apply_schema_change(
        &self,
        _name: &SchemaName,
        _steps: &[MigrationStep],
        _definition: Option<&SchemaDefinition>,
    ) -> impl Future<Output = Result<(), BackendError>> + Send {
        async {
            Err(BackendError::MigrationFailed {
                step: "atomic schema change".into(),
                reason: "backend does not support atomic schema changes".into(),
            })
        }
    }

    /// Finish cross-schema constraints after applying a batch, including legacy repair.
    /// Called only during explicit schema administration, never on a read connection.
    fn finalize_schema_migrations(&self) -> impl Future<Output = Result<(), BackendError>> + Send {
        async { Ok(()) }
    }

    /// Whether explicit preparation of record revisions is available.
    fn supports_record_revisions(&self) -> bool {
        false
    }

    /// Explicitly backfill and enable record revisions for an applied schema.
    fn prepare_record_revisions(
        &self,
        _schema: &SchemaName,
    ) -> impl Future<Output = Result<(), BackendError>> + Send {
        async {
            Err(BackendError::MigrationFailed {
                step: "prepare record revisions".into(),
                reason: "backend does not support record revisions".into(),
            })
        }
    }

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
    /// Atomically delete internal invitation candidates only if their lifecycle
    /// status and original timestamp still match. Return committed deletion count.
    fn prune_invitations(
        &self,
        _candidates: &[crate::invite_store::InvitationPruneCandidate],
    ) -> impl Future<Output = Result<u64, BackendError>> + Send {
        async {
            Err(BackendError::QueryError {
                message: "invitation retention is unsupported by this backend".into(),
            })
        }
    }

    /// Optional durable create reconciliation; unsupported adapters must refuse.
    fn create_intent(
        &self,
        _request: &crate::create_intent::CreateIntentRequest,
    ) -> impl Future<
        Output = Result<
            crate::create_intent::CreateIntentReceipt,
            crate::create_intent::CreateIntentError,
        >,
    > + Send {
        async { Err(crate::create_intent::CreateIntentError::Unsupported) }
    }

    /// Atomically commit a create intent together with creator membership.
    /// Committed receipts reconcile without inserting another membership.
    fn create_intent_with_membership(
        &self,
        _request: &crate::create_intent::CreateIntentRequest,
        _membership: &Entity,
    ) -> impl Future<
        Output = Result<
            crate::create_intent::CreateIntentReceipt,
            crate::create_intent::CreateIntentError,
        >,
    > + Send {
        async { Err(crate::create_intent::CreateIntentError::Unsupported) }
    }

    /// Create a tenant root and its creator membership in one transaction.
    /// Adapters must fail without writing if atomic creation is unsupported.
    fn create_with_membership(
        &self,
        _entity: &Entity,
        _membership: &Entity,
    ) -> impl Future<Output = Result<Entity, BackendError>> + Send {
        async {
            Err(BackendError::QueryError {
                message: "atomic creator membership is unsupported by this backend".into(),
            })
        }
    }

    /// Read an entity and opaque revision from a single consistent snapshot.
    /// Unsupported backends must not synthesize revisions from ordinary reads.
    fn get_versioned(
        &self,
        _schema: &SchemaName,
        _id: &EntityId,
    ) -> impl Future<
        Output = Result<
            crate::conditional::VersionedEntity,
            crate::conditional::ConditionalMutationError,
        >,
    > + Send {
        async { Err(crate::conditional::ConditionalMutationError::Unsupported) }
    }

    /// Atomically update only the authorized expected record revision.
    fn update_if(
        &self,
        _entity: &Entity,
        _expected: &crate::conditional::EntityRevision,
    ) -> impl Future<
        Output = Result<
            crate::conditional::VersionedEntity,
            crate::conditional::ConditionalMutationError,
        >,
    > + Send {
        async { Err(crate::conditional::ConditionalMutationError::Unsupported) }
    }

    /// Atomically delete only the authorized expected record revision.
    fn delete_if(
        &self,
        _schema: &SchemaName,
        _id: &EntityId,
        _expected: &crate::conditional::EntityRevision,
    ) -> impl Future<Output = Result<(), crate::conditional::ConditionalMutationError>> + Send {
        async { Err(crate::conditional::ConditionalMutationError::Unsupported) }
    }

    /// Create a new entity in the backend.
    ///
    /// The entity's `id` and `schema` determine where it is stored.
    /// Returns the created entity (which may have backend-generated fields).
    fn create(&self, entity: &Entity) -> impl Future<Output = Result<Entity, BackendError>> + Send;

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
    /// Supplied fields replace their corresponding values; omitted fields remain unchanged.
    /// An explicit `DynamicValue::Null` writes null rather than omitting the field.
    /// Returns the updated entity.
    fn update(&self, entity: &Entity) -> impl Future<Output = Result<Entity, BackendError>> + Send;

    /// Atomically update one field only if its value still matches the snapshot.
    ///
    /// Returns `false` when the row is absent, the field has changed, or a
    /// concurrent write prevents the conditional update from committing. Other
    /// fields must be preserved. Missing fields compare equal to `DynamicValue::Null`.
    /// Custom backends fail closed unless implemented.
    fn update_field_if_matches(
        &self,
        _schema: &SchemaName,
        _id: &EntityId,
        _field: &FieldName,
        _expected: &DynamicValue,
        _value: &DynamicValue,
    ) -> impl Future<Output = Result<bool, BackendError>> + Send {
        async {
            Err(BackendError::QueryError {
                message: "backend does not support atomic field updates".into(),
            })
        }
    }

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

    /// Execute a proven row-independent Cedar read without decoding all matches.
    ///
    /// The caller must prove the policy decision and strict Cedar schema shape
    /// against one frozen authorization snapshot. The backend must certify that
    /// every stored resource matches `expected_schema` and the caller's Cedar
    /// representation, including physical tenant columns and required values.
    /// `Some` guarantees equivalent tenant filtering, pagination, and exact totals
    /// when requested. Projections and unproven shapes return `None`; real I/O
    /// failures return an error. Custom backends remain unsupported by default.
    fn query_cedar_compatible(
        &self,
        _expected_schema: &SchemaDefinition,
        _query: &Query,
        _scope: &crate::auth::CedarReadScope,
    ) -> impl Future<Output = Result<Option<QueryResult>, BackendError>> + Send {
        async { Ok(None) }
    }

    /// Count entities matching a query (ignoring limit/offset).
    ///
    /// Returns the total number of entities that match the query's schema
    /// and filter criteria. Limit and offset are not applied.
    fn count(&self, query: &Query) -> impl Future<Output = Result<usize, BackendError>> + Send;

    /// Compute aggregate values over entities matching a query.
    ///
    /// Returns one `AggregateResult` per operation in the query's `ops` list.
    fn aggregate(
        &self,
        query: &AggregateQuery,
    ) -> impl Future<Output = Result<Vec<AggregateResult>, BackendError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnsupportedStore(std::sync::atomic::AtomicUsize);

    impl EntityStore for UnsupportedStore {
        async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(entity.clone())
        }
        async fn get(&self, _: &SchemaName, _: &EntityId) -> Result<Entity, BackendError> {
            panic!("unsupported atomic operation must not read")
        }
        async fn update(&self, _: &Entity) -> Result<Entity, BackendError> {
            panic!("unsupported atomic operation must not update")
        }
        async fn delete(&self, _: &SchemaName, _: &EntityId) -> Result<(), BackendError> {
            panic!("unsupported atomic operation must not compensate")
        }
        async fn query(&self, _: &Query) -> Result<QueryResult, BackendError> {
            panic!("unsupported atomic operation must not query")
        }
        async fn count(&self, _: &Query) -> Result<usize, BackendError> {
            panic!("unsupported atomic operation must not count")
        }
        async fn aggregate(
            &self,
            _: &AggregateQuery,
        ) -> Result<Vec<AggregateResult>, BackendError> {
            panic!("unsupported atomic operation must not aggregate")
        }
    }

    #[tokio::test]
    async fn unsupported_creator_membership_never_performs_partial_writes() {
        use crate::create_intent::{
            CreateIntentError, CreateIntentId, CreateIntentRequest, CreateIntentScope,
        };
        use schema_forge_core::types::SchemaId;
        let store = UnsupportedStore(std::sync::atomic::AtomicUsize::new(0));
        let schema = SchemaName::new("Organization").unwrap();
        let entity = Entity::new(schema.clone(), std::collections::BTreeMap::new());
        let membership = Entity::new(
            SchemaName::new("TenantMembership").unwrap(),
            std::collections::BTreeMap::new(),
        );
        assert!(store
            .create_with_membership(&entity, &membership)
            .await
            .is_err());
        let request = CreateIntentRequest::Read {
            scope: CreateIntentScope {
                principal: "owner".into(),
                tenant: String::new(),
                schema: SchemaDefinition::new(
                    SchemaId::new(),
                    schema,
                    vec![schema_forge_core::types::FieldDefinition::new(
                        FieldName::new("name").unwrap(),
                        schema_forge_core::types::FieldType::Text(
                            schema_forge_core::types::TextConstraints::unconstrained(),
                        ),
                    )],
                    vec![],
                )
                .unwrap(),
            },
            id: CreateIntentId::fresh(),
        };
        assert_eq!(
            store
                .create_intent_with_membership(&request, &membership)
                .await
                .unwrap_err(),
            CreateIntentError::Unsupported
        );
        assert_eq!(store.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    // Compile-time verification that traits have the correct bounds.
    // These functions are never called -- they just verify the trait is object-safe enough
    // for RPITIT usage and that Send + Sync is required.
    fn _assert_schema_backend_send_sync<T: SchemaBackend>() {}
    fn _assert_entity_store_send_sync<T: EntityStore>() {}
}
