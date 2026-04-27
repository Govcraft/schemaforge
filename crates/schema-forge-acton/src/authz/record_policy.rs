//! Cedar-backed implementation of [`RecordAccessPolicy`].
//!
//! Replaces the old `OwnershipBasedPolicy` shipped from `schema-forge-backend`.
//! Decisions flow through the same Cedar engine that handles schema-level
//! checks: `filter_visible` runs each candidate row through `authorize` with
//! the row as resource, `can_modify` and `can_delete` evaluate the matching
//! Cedar action with the entity as resource. The trait abstraction
//! [`schema_forge_backend::auth::RecordAccessPolicy`] stays in the lower
//! crate as the public extension point for operators who want to plug in
//! a custom policy implementation.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use acton_service::middleware::Claims;
use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::SchemaDefinition;

use crate::authz::engine::authorize;
use crate::authz::namespace::ActionVerb;
use crate::authz::store::PolicyStore;

/// Cedar-backed record-level policy.
///
/// Holds an `Arc<PolicyStore>` so every decision evaluates against the
/// current compiled bundle. Hot-swapped when schemas change without
/// invalidating any in-flight references.
pub struct CedarRecordPolicy {
    store: Arc<PolicyStore>,
}

impl CedarRecordPolicy {
    /// Constructs a new policy backed by `store`.
    pub fn new(store: Arc<PolicyStore>) -> Self {
        Self { store }
    }
}

impl RecordAccessPolicy for CedarRecordPolicy {
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>> {
        Box::pin(async move {
            let mut visible = Vec::with_capacity(entities.len());
            for entity in entities {
                match authorize(
                    &self.store,
                    Some(claims),
                    ActionVerb::Read,
                    schema,
                    Some(&entity),
                ) {
                    Ok(d) if d.is_allow() => visible.push(entity),
                    // Deny or evaluation error — drop. Engine already audited.
                    _ => {}
                }
            }
            visible
        })
    }

    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            authorize(
                &self.store,
                Some(claims),
                ActionVerb::Update,
                schema,
                Some(entity),
            )
            .map(|d| d.is_allow())
            .unwrap_or(false)
        })
    }

    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            authorize(
                &self.store,
                Some(claims),
                ActionVerb::Delete,
                schema,
                Some(entity),
            )
            .map(|d| d.is_allow())
            .unwrap_or(false)
        })
    }
}
