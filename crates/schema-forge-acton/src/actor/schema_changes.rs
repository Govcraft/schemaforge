//! Serialized storage/registry commits using immutable preflight policy bundles.

use std::sync::Arc;

use acton_service::prelude::{Idle, ManagedActor, Reply};
use schema_forge_core::types::{SchemaDefinition, SchemaName};

use super::ForgeActor;
use crate::{error::ForgeError, messages::ApplyPreparedSchemaChange};

#[derive(Debug)]
struct SchemaChangeFailure {
    name: SchemaName,
    previous: Option<SchemaDefinition>,
    cause: ForgeError,
}

impl std::fmt::Display for SchemaChangeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.cause, f)
    }
}

impl std::error::Error for SchemaChangeFailure {}

pub(super) fn configure(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.try_mutate_on::<ApplyPreparedSchemaChange, (), SchemaChangeFailure>(|actor, context| {
        let change = context.message().clone();
        let store = actor.model.policy_store.clone();
        let unchanged = actor.model.registry == change.expected_registry
            && store
                .as_ref()
                .is_some_and(|store| Arc::ptr_eq(&store.current(), &change.expected_policy));
        if !unchanged {
            return Reply::try_pending(async move {
                change
                    .reply
                    .send(Err(ForgeError::Conflict {
                        reason: "schema_preflight_stale",
                        message:
                            "schemas or policies changed after validation; retry the schema change"
                                .into(),
                    }))
                    .await;
                Ok(())
            });
        }
        let (Some(backend), Some(store)) = (actor.model.backend.clone(), store) else {
            return Reply::try_pending(async move {
                change
                    .reply
                    .send(Err(ForgeError::Internal {
                        message: "schema backend is not initialized".into(),
                    }))
                    .await;
                Ok(())
            });
        };
        let name = change.definition.name.clone();
        // Mutable handler futures are awaited inline. No subsequent actor
        // message observes this provisional registry until storage succeeds,
        // or the on_error handler below has restored it.
        let previous = if change.remove {
            actor.model.registry.remove(name.as_str())
        } else {
            actor
                .model
                .registry
                .insert(name.to_string(), change.definition.clone())
        };
        Reply::try_pending(async move {
            let result = async {
                if !change.steps.is_empty() {
                    backend.apply_migration(&name, &change.steps).await?;
                }
                if !change.remove {
                    backend.store_schema_metadata(&change.definition).await?;
                    backend.finalize_schema_migrations().await?;
                }
                Ok::<(), schema_forge_backend::BackendError>(())
            }
            .await;
            if let Err(error) = result {
                return Err(SchemaChangeFailure {
                    name,
                    previous,
                    cause: error.into(),
                });
            }
            store.swap_prepared(change.next_policy);
            change.reply.send(Ok(())).await;
            Ok(())
        })
    });
    actor.on_error::<ApplyPreparedSchemaChange, SchemaChangeFailure>(|actor, context, failure| {
        if let Some(previous) = &failure.previous {
            actor
                .model
                .registry
                .insert(failure.name.to_string(), previous.clone());
        } else {
            actor.model.registry.remove(failure.name.as_str());
        }
        let reply = context.message().reply.clone();
        let error = failure.cause.clone();
        Reply::pending(async move { reply.send(Err(error)).await })
    });
}
