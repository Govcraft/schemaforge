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

use crate::hooks::HookDispatcher;
use crate::messages::{
    AggregateEntities, ApplyMigration, CountEntities, CreateEntity, DeleteEntity, GetEntity,
    GetHookDispatcher, GetRecordAccessPolicy, GetSchema, GetSchemasBatch, GetStorageRegistry,
    GetTenantConfig, InitForge, InsertSchema, ListSchemas, LoadSchemaMetadata, QueryEntities,
    RemoveSchema, StoreSchemaMetadata, UpdateEntity, UpdateTenantConfig,
};
use crate::state::DynForgeBackend;
use crate::storage::StorageRegistry;

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
    pub(crate) hook_dispatcher: Option<Arc<dyn HookDispatcher>>,
    pub(crate) storage_registry: StorageRegistry,
    pub(crate) policy_store: Option<Arc<crate::authz::PolicyStore>>,
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
            .field(
                "hook_dispatcher",
                &self.hook_dispatcher.as_ref().map(|_| ".."),
            )
            .field("storage_backends", &self.storage_registry.len())
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
            hook_dispatcher: None,
            storage_registry: StorageRegistry::default(),
            policy_store: None,
        }
    }
}

impl ActorExtension for ForgeActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        configure_init(actor);
        configure_registry_reads(actor);
        configure_registry_mutations(actor);
        configure_backend_operations(actor);
    }
}

// ---------------------------------------------------------------------------
// Initialization (sent once after spawning, before serving)
// ---------------------------------------------------------------------------

fn configure_init(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.mutate_on::<InitForge>(|actor, ctx| {
        let msg = ctx.message();
        debug!(
            "initializing ForgeActor with {} schemas",
            msg.registry.len()
        );
        actor.model.registry = msg.registry.clone();
        actor.model.backend = Some(msg.backend.clone());
        actor.model.tenant_config = msg.tenant_config.clone();
        actor.model.hook_dispatcher = msg.hook_dispatcher.clone();
        actor.model.storage_registry = msg.storage_registry.clone();

        // Lazy-init the policy store when the caller did not supply one. The
        // CLI / extension build paths always pass it, but tests and ad-hoc
        // actor wiring may set `policy_store: None`. We compile a default
        // store from the registered schemas so every authz path has a
        // non-degenerate store to evaluate against.
        actor.model.policy_store = msg.policy_store.clone().or_else(|| {
            let schemas: Vec<schema_forge_core::types::SchemaDefinition> =
                msg.registry.values().cloned().collect();
            match crate::authz::store::PolicyStoreSnapshot::from_schemas(
                &schemas,
                None,
                crate::authz::role_ranks::RoleRanks::empty(),
                crate::authz::principal_claims::PrincipalClaimMappings::default(),
            ) {
                Ok(snapshot) => Some(Arc::new(crate::authz::PolicyStore::new(snapshot))),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "InitForge could not lazy-compile a default Cedar policy store; \
                         every authz check will fail until a valid store is supplied"
                    );
                    None
                }
            }
        });

        // Default the record-access policy to the Cedar-backed implementation
        // whenever the caller did not supply one. This mirrors the
        // `SchemaForgeExtension::build()` path so the actor flow used by
        // tests and the CLI is also Cedar-canonical by default.
        actor.model.record_access_policy = msg.record_access_policy.clone().or_else(|| {
            actor.model.policy_store.clone().map(|store| {
                std::sync::Arc::new(crate::authz::CedarRecordPolicy::new(store))
                    as std::sync::Arc<dyn schema_forge_backend::auth::RecordAccessPolicy>
            })
        });
        let reply = msg.reply.clone();
        Reply::pending(async move {
            reply.send(()).await;
        })
    });
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

    actor.act_on::<GetSchemasBatch>(|actor, ctx| {
        let msg = ctx.message();
        let mut found: HashMap<String, SchemaDefinition> = HashMap::new();
        for name in &msg.names {
            if let Some(def) = actor.model.registry.get(name) {
                found.insert(name.clone(), def.clone());
            }
        }
        let reply = msg.reply.clone();
        Reply::pending(async move {
            reply.send(found).await;
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

    actor.act_on::<GetHookDispatcher>(|actor, ctx| {
        let dispatcher = actor.model.hook_dispatcher.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(dispatcher).await;
        })
    });

    actor.act_on::<crate::messages::GetPolicyStore>(|actor, ctx| {
        let store = actor.model.policy_store.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(store).await;
        })
    });

    actor.act_on::<GetStorageRegistry>(|actor, ctx| {
        let registry = actor.model.storage_registry.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(registry).await;
        })
    });
}

// ---------------------------------------------------------------------------
// Registry mutations (mutable, some with reply channels)
// ---------------------------------------------------------------------------

fn configure_registry_mutations(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.mutate_on::<InsertSchema>(|actor, ctx| {
        let msg = ctx.message();
        let name = msg.name.clone();
        let definition = msg.definition.clone();
        debug!(schema = %name, "inserting schema into registry");

        // Tentatively install the new definition so the recompile sees the
        // proposed registry state.
        let previous = actor.model.registry.insert(name.clone(), definition);

        let result = recompile_policy_store(&mut actor.model, &name);
        if result.is_err() {
            // Roll back the registry mutation so the live bundle and
            // registry stay in sync.
            match previous {
                Some(prev) => {
                    actor.model.registry.insert(name.clone(), prev);
                }
                None => {
                    actor.model.registry.remove(&name);
                }
            }
        }

        let reply = msg.reply.clone();
        Reply::pending(async move {
            reply.send(result).await;
        })
    });

    actor.mutate_on::<RemoveSchema>(|actor, ctx| {
        let name = ctx.message().name.clone();
        debug!(schema = %name, "removing schema from registry");
        let removed = actor.model.registry.remove(&name);

        let result = match recompile_policy_store(&mut actor.model, &name) {
            Ok(()) => Ok(removed),
            Err(e) => {
                // Roll back the removal.
                if let Some(prev) = removed {
                    actor.model.registry.insert(name.clone(), prev);
                }
                Err(e)
            }
        };

        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(result).await;
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
// Policy-store recompile
// ---------------------------------------------------------------------------

/// Recompile the actor's Cedar [`PolicyStore`] from the current registry,
/// atomically swapping the active snapshot on success. The actor field is
/// left untouched; only the `ArcSwap` inside the existing store is updated
/// so every outstanding `Arc<PolicyStore>` clone — including the ones held
/// by `ForgeState` and live request handlers — sees the new bundle on the
/// next `current()` call.
///
/// Returns the formatted error string on compile failure so handlers can
/// surface it through their reply channel without leaking a Cedar error
/// type into the public message API.
fn recompile_policy_store(
    model: &mut ForgeActor,
    mutating_schema: &str,
) -> std::result::Result<(), String> {
    let store = match model.policy_store.as_ref() {
        Some(store) => store.clone(),
        None => {
            // No store yet (test or pre-init flow). Nothing to recompile —
            // InitForge will compile on its first run.
            return Ok(());
        }
    };

    let schemas: Vec<SchemaDefinition> = model.registry.values().cloned().collect();
    match store.recompile_from_schemas(&schemas, None) {
        Ok(()) => {
            tracing::info!(
                target: "schema_forge_acton::authz",
                schema = mutating_schema,
                policy_count = store.current().policy_count,
                policy_hash = %store.current().policy_hash,
                "policy_store recompiled after registry mutation"
            );
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            tracing::error!(
                target: "schema_forge_acton::authz",
                schema = mutating_schema,
                error = %msg,
                "policy_store recompile failed; reverting registry mutation"
            );
            Err(msg)
        }
    }
}

// ---------------------------------------------------------------------------
// Backend operations
//
// `act_on` handlers run with shared (read-only) access to actor state and
// are scheduled concurrently by the acton runtime, so awaiting backend
// calls directly here is the idiomatic pattern. We deliberately do **not**
// wrap these calls in `tokio::spawn` — that would orphan the work from
// acton's supervision tree (no backpressure, no graceful shutdown
// coordination, and historically the source of issue #11 self-deadlocks
// when an `after_change` hook tried to write back to the trigger entity).
//
// The boxed futures returned by `DynEntityStore` / `DynSchemaBackend`
// are wrapped in `sync_wrapper::SyncFuture` at the trait boundary so they
// satisfy acton-reactive's `Send + Sync` `FutureBox` bound (see
// `crate::state` for details).
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
                Some(b) => b.create(&entity).await,
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
                Some(b) => b.get(&schema, &id).await,
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
                Some(b) => b.update(&entity).await,
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
                Some(b) => b.delete(&schema, &id).await,
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
                Some(b) => b.query(&query).await,
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
                Some(b) => b.count(&query).await,
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
                Some(b) => b.aggregate(&query).await,
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
                Some(b) => b.apply_migration(&schema_name, &steps).await,
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
                Some(b) => b.store_schema_metadata(&definition).await,
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
                Some(b) => b.load_schema_metadata(&name).await,
                None => {
                    warn!("LoadSchemaMetadata received but no backend is configured");
                    Err(no_backend_error())
                }
            };
            reply.send(result).await;
        })
    });
}
