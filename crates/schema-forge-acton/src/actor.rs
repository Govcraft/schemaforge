//! The `ForgeActor` — an acton-reactive actor extension that owns
//! SchemaForge's runtime state: the schema registry, database backend,
//! tenant configuration, and record-access policy.
//!
//! Handlers interact with the actor via message passing (see [`crate::messages`]).
//! Request-response messages embed a [`ReplyChannel`](crate::messages::ReplyChannel)
//! that the handler uses to send the result back to the caller.

use std::collections::HashMap;
use std::sync::Arc;

use acton_service::prelude::*;
use tracing::{debug, warn};

use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::error::BackendError;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::types::SchemaDefinition;

use crate::messages::{
    AggregateEntities, ApplyMigration, CountEntities, CreateEntity, DeleteEntity, GetEntity,
    GetRecordAccessPolicy, GetSchema, GetTenantConfig, InsertSchema, ListSchemas,
    LoadSchemaMetadata, QueryEntities, RemoveSchema, StoreSchemaMetadata, UpdateEntity,
    UpdateTenantConfig,
};
use crate::state::DynForgeBackend;

// ---------------------------------------------------------------------------
// ForgeActor
// ---------------------------------------------------------------------------

/// Actor extension that owns SchemaForge's runtime state.
///
/// The `backend` field is `Option` because `ActorExtension` requires `Default`.
/// After construction via [`ForgeActor::with_backend`], it is always `Some`.
#[derive(Default)]
pub struct ForgeActor {
    pub(crate) registry: HashMap<String, SchemaDefinition>,
    pub(crate) backend: Option<Arc<dyn DynForgeBackend>>,
    pub(crate) tenant_config: Option<TenantConfig>,
    pub(crate) record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
}

impl std::fmt::Debug for ForgeActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgeActor")
            .field("registry", &self.registry)
            .field("backend", &self.backend.as_ref().map(|_| ".."))
            .field("tenant_config", &self.tenant_config)
            .field(
                "record_access_policy",
                &self.record_access_policy.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

impl ForgeActor {
    /// Create a new `ForgeActor` with the given backend.
    ///
    /// Use this to pre-populate the actor's state before registration,
    /// or set the backend after the actor is spawned via a lifecycle hook.
    pub fn with_backend(backend: Arc<dyn DynForgeBackend>) -> Self {
        Self {
            registry: HashMap::new(),
            backend: Some(backend),
            tenant_config: None,
            record_access_policy: None,
        }
    }
}

impl ActorExtension for ForgeActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        configure_registry_reads(actor);
        configure_registry_mutations(actor);
        configure_backend_operations(actor);
    }
}

// ---------------------------------------------------------------------------
// Registry reads (read-only — reply via oneshot channel)
// ---------------------------------------------------------------------------

fn configure_registry_reads(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.act_on::<GetSchema>(|actor, ctx| {
        let name = &ctx.message().name;
        let result = actor.model.registry.get(name).cloned();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(result).await;
        })
    });

    actor.act_on::<ListSchemas>(|actor, ctx| {
        let schemas: Vec<SchemaDefinition> = actor.model.registry.values().cloned().collect();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(schemas).await;
        })
    });

    actor.act_on::<GetTenantConfig>(|actor, ctx| {
        let config = actor.model.tenant_config.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(config).await;
        })
    });

    actor.act_on::<GetRecordAccessPolicy>(|actor, ctx| {
        let policy = actor.model.record_access_policy.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(policy).await;
        })
    });
}

// ---------------------------------------------------------------------------
// Registry mutations (mutable, some with reply channels)
// ---------------------------------------------------------------------------

fn configure_registry_mutations(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.mutate_on::<InsertSchema>(|actor, ctx| {
        let msg = ctx.message();
        debug!(schema = %msg.name, "inserting schema into registry");
        actor
            .model
            .registry
            .insert(msg.name.clone(), msg.definition.clone());
        Reply::ready()
    });

    actor.mutate_on::<RemoveSchema>(|actor, ctx| {
        let name = &ctx.message().name;
        debug!(schema = %name, "removing schema from registry");
        let removed = actor.model.registry.remove(name);
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(removed).await;
        })
    });

    actor.mutate_on::<UpdateTenantConfig>(|actor, ctx| {
        let config = ctx.message().config.clone();
        debug!("updating tenant config");
        actor.model.tenant_config = config;
        Reply::ready()
    });
}

// ---------------------------------------------------------------------------
// Backend operations (read-only actor access, async backend calls)
//
// The DynForgeBackend trait methods return `Pin<Box<dyn Future + Send>>` (not
// `+ Sync`), but acton-reactive's `FutureBox` requires `Send + Sync`. We
// bridge this by spawning the backend call on a tokio task and awaiting the
// `JoinHandle`, which is both `Send` and `Sync`.
// ---------------------------------------------------------------------------

/// Helper: return a "no backend configured" error.
fn no_backend_error() -> BackendError {
    BackendError::Internal {
        message: "no backend configured".into(),
    }
}

fn configure_backend_operations(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.act_on::<CreateEntity>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let entity = ctx.message().entity.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.create(&entity).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("CreateEntity received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<GetEntity>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let schema = ctx.message().schema.clone();
        let id = ctx.message().id.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.get(&schema, &id).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("GetEntity received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<UpdateEntity>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let entity = ctx.message().entity.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.update(&entity).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("UpdateEntity received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<DeleteEntity>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let schema = ctx.message().schema.clone();
        let id = ctx.message().id.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.delete(&schema, &id).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("DeleteEntity received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<QueryEntities>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let query = ctx.message().query.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.query(&query).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("QueryEntities received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<CountEntities>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let query = ctx.message().query.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.count(&query).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("CountEntities received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<AggregateEntities>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let query = ctx.message().query.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => tokio::spawn(async move { b.aggregate(&query).await })
                    .await
                    .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() })),
                None => {
                    warn!("AggregateEntities received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<ApplyMigration>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let schema_name = ctx.message().schema_name.clone();
        let steps = ctx.message().steps.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => {
                    tokio::spawn(async move { b.apply_migration(&schema_name, &steps).await })
                        .await
                        .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() }))
                }
                None => {
                    warn!("ApplyMigration received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<StoreSchemaMetadata>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let definition = ctx.message().definition.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => {
                    tokio::spawn(async move { b.store_schema_metadata(&definition).await })
                        .await
                        .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() }))
                }
                None => {
                    warn!("StoreSchemaMetadata received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });

    actor.act_on::<LoadSchemaMetadata>(|actor, ctx| {
        let backend = actor.model.backend.clone();
        let name = ctx.message().name.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            let result = match backend {
                Some(b) => {
                    tokio::spawn(async move { b.load_schema_metadata(&name).await })
                        .await
                        .unwrap_or_else(|e| Err(BackendError::Internal { message: e.to_string() }))
                }
                None => {
                    warn!("LoadSchemaMetadata received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });
}
