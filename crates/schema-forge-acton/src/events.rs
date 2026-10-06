//! Process-local, bounded committed change delivery. Raw snapshots stay internal.
use crate::{
    actor::ForgeActor, events_config::EventsConfig, messages::*, webhook::WebhookEvent,
    DynAuthStore,
};
use acton_service::{
    prelude::{ActonMessage, ActorHandle, ActorHandleInterface, Idle, ManagedActor, Reply},
    sse::{BroadcastMessage, ConnectionId, SseBroadcaster},
};
use schema_forge_backend::{DynEntityStore, Entity};
use schema_forge_core::types::{DynamicValue, EntityId, SchemaName};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, OnceLock, Weak},
};
use tokio::sync::{broadcast, oneshot};

/// Shared services. Mutable connection admission and commit ordering belong to ForgeActor.
pub struct EventsRuntime {
    pub config: EventsConfig,
    pub(crate) broadcaster: SseBroadcaster,
    pub(crate) auth_store: Arc<dyn DynAuthStore>,
    pub(crate) entity_store: Arc<dyn DynEntityStore>,
}
impl std::fmt::Debug for EventsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventsRuntime")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
impl EventsRuntime {
    /// Validate before allocating the bounded broadcast ring.
    pub fn new(
        config: EventsConfig,
        auth_store: Arc<dyn DynAuthStore>,
        entity_store: Arc<dyn DynEntityStore>,
    ) -> Result<Self, crate::events_config::EventsConfigError> {
        let mut bounds = config.clone();
        bounds.enabled = true;
        bounds.validate()?;
        Ok(Self {
            broadcaster: SseBroadcaster::with_capacity(config.channel_capacity),
            config,
            auth_store,
            entity_store,
        })
    }
    fn publish(&self, event: &WebhookEvent, entity: &Entity) {
        let snapshot = CommittedSnapshot {
            event_id: event.event_id.clone(),
            event_type: event.event_type.clone(),
            schema: entity.schema.to_string(),
            entity_id: entity.id.clone(),
            timestamp: event.timestamp.clone(),
            actor: event.actor.clone(),
            fields: entity.fields.clone(),
        };
        match BroadcastMessage::json(&snapshot) {
            Ok(message) => {
                let _ = self.broadcaster.broadcast(message);
            }
            Err(error) => tracing::error!(%error, "committed event serialization failed"),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CommittedSnapshot {
    pub event_id: String,
    pub event_type: String,
    pub schema: String,
    pub entity_id: EntityId,
    pub timestamp: String,
    pub actor: Option<String>,
    pub fields: BTreeMap<String, DynamicValue>,
}
impl CommittedSnapshot {
    pub fn entity(&self) -> Result<Entity, crate::error::ForgeError> {
        let schema = SchemaName::new(&self.schema).map_err(|_| {
            crate::error::ForgeError::InvalidSchemaName {
                name: self.schema.clone(),
            }
        })?;
        Ok(Entity::with_id(
            self.entity_id.clone(),
            schema,
            self.fields.clone(),
        ))
    }
}
#[derive(Debug, Default)]
pub(crate) struct ActorEvents {
    runtime: Option<Arc<EventsRuntime>>,
    configured: bool,
    connections: HashMap<ConnectionId, (String, Weak<()>)>,
}
/// Configure once after InitForge, before serving requests.
#[derive(Debug, Clone)]
pub struct ConfigureEvents {
    pub runtime: Arc<EventsRuntime>,
    pub reply: ReplyChannel<()>,
}
#[derive(Debug, Clone)]
pub(crate) struct GetEvents {
    pub reply: ReplyChannel<Option<Arc<EventsRuntime>>>,
}
#[derive(Debug, Clone)]
pub(crate) struct Subscribe {
    pub subject: String,
    pub reply: ReplyChannel<Result<Subscription, crate::error::ForgeError>>,
}
pub(crate) struct Subscription {
    pub receiver: broadcast::Receiver<BroadcastMessage>,
    pub _lease: Arc<()>,
}
/// Shared metadata lets webhooks reuse the exact commit event ID and timestamp.
#[derive(Debug, Clone, Default)]
pub(crate) struct Notification {
    pub subject: Option<String>,
    pub deleted: Option<Entity>,
    pub event: Arc<OnceLock<WebhookEvent>>,
}
#[derive(Debug, Clone)]
pub(crate) struct EventMutation<M: Clone + std::fmt::Debug> {
    pub mutation: M,
    pub notification: Notification,
}

pub(crate) async fn runtime(
    handle: &ActorHandle,
) -> Result<Option<Arc<EventsRuntime>>, crate::error::ForgeError> {
    let (tx, rx) = oneshot::channel();
    handle
        .send(GetEvents {
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| crate::error::ForgeError::Internal {
            message: "Forge actor timeout".into(),
        })?
        .map_err(|_| crate::error::ForgeError::Internal {
            message: "Forge actor unavailable".into(),
        })
}
/// Only enabled event writes use the serialized envelope; all replies keep their existing types.
pub(crate) async fn send<M: ActonMessage + Clone + std::fmt::Debug>(
    handle: &ActorHandle,
    mutation: M,
    notification: &Notification,
) -> Result<(), crate::error::ForgeError> {
    if runtime(handle).await?.is_some() {
        handle
            .send(EventMutation {
                mutation,
                notification: notification.clone(),
            })
            .await;
    } else {
        handle.send(mutation).await;
    }
    Ok(())
}
pub(crate) fn configure_actor(actor: &mut ManagedActor<Idle, ForgeActor>) {
    actor.mutate_on::<ConfigureEvents>(|actor, ctx| {
        if !actor.model.events.configured {
            actor.model.events.runtime = ctx
                .message()
                .runtime
                .config
                .enabled
                .then(|| ctx.message().runtime.clone());
            actor.model.events.configured = true;
        } else {
            tracing::warn!("EventsRuntime already configured; preserving the process broadcaster");
        }
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(()).await;
        })
    });
    actor.act_on::<GetEvents>(|actor, ctx| {
        let runtime = actor.model.events.runtime.clone();
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(runtime).await;
        })
    });
    actor.mutate_on::<Subscribe>(|actor, ctx| {
        let events = &mut actor.model.events;
        events
            .connections
            .retain(|_, (_, lifetime)| lifetime.strong_count() > 0);
        let subject = &ctx.message().subject;
        let result = match &events.runtime {
            Some(runtime)
                if events
                    .connections
                    .values()
                    .filter(|(user, _)| user == subject)
                    .count()
                    < runtime.config.max_connections_per_user =>
            {
                let lease = Arc::new(());
                events.connections.insert(
                    ConnectionId::new(),
                    (subject.clone(), Arc::downgrade(&lease)),
                );
                Ok(Subscription {
                    receiver: runtime.broadcaster.subscribe(),
                    _lease: lease,
                })
            }
            Some(_) => Err(crate::error::ForgeError::RateLimited {
                message: "Too many event connections.".into(),
            }),
            None => Err(crate::error::ForgeError::EntityNotFound {
                schema: "events".into(),
                entity_id: "disabled".into(),
            }),
        };
        let reply = ctx.message().reply.clone();
        Reply::pending(async move {
            reply.send(result).await;
        })
    });
    // mutable handlers await commits inline, including publication, so cancellation
    // of the HTTP reply receiver cannot interrupt or reorder a committed event.
    macro_rules! write_handler {
        ($message:ty, $operation:expr, $snapshot:expr, $kind:literal) => {
            actor.mutate_on::<EventMutation<$message>>(|actor, ctx| {
                let backend = actor.model.backend.clone();
                let runtime = actor.model.events.runtime.clone();
                let schemas = actor.model.registry.clone();
                let mut envelope = ctx.message().clone();
                Reply::pending(async move {
                    if let Some(backend) = backend {
                        let request = &envelope.mutation;
                        if $kind == "deleted" {
                            if let Some(previous) = &envelope.notification.deleted {
                                // Capture the actual pre-delete record inside the serialized commit.
                                match backend.get(&previous.schema, &previous.id).await {
                                    Ok(snapshot) => envelope.notification.deleted = Some(snapshot),
                                    Err(error) => {
                                        tracing::warn!(%error, "pre-delete snapshot unavailable; no event will be published");
                                        envelope.notification.deleted = None;
                                    },
                                }
                            }
                        }
                        let result = ($operation)(backend.clone(), request.clone()).await;
                        if let Ok(value) = &result {
                            if let Some(mut entity) =
                                ($snapshot)(value, request, &envelope.notification)
                            {
                                if $kind == "created" {
                                    match backend.get(&entity.schema, &entity.id).await {
                                        Ok(stored) => entity = stored,
                                        Err(error) => {
                                            tracing::warn!(%error, "committed create snapshot unavailable; no event will be published");
                                            envelope.mutation.reply.send(result).await;
                                            return;
                                        }
                                    }
                                }
                                if let (Some(runtime), Some(schema)) =
                                    (runtime, schemas.get(entity.schema.as_str()))
                                {
                                    let subject = envelope.notification.subject.as_deref();
                                    let event = match $kind {
                                        "created" => {
                                            WebhookEvent::from_create(schema, &entity, subject)
                                        }
                                        "deleted" => WebhookEvent::from_delete(
                                            entity.schema.as_str(),
                                            entity.id.as_str(),
                                            subject,
                                        ),
                                        _ => WebhookEvent::from_update(schema, &entity, subject),
                                    };
                                    runtime.publish(&event, &entity);
                                    let _ = envelope.notification.event.set(event);
                                }
                            }
                        }
                        envelope.mutation.reply.send(result).await;
                    } else {
                        tracing::error!("event mutation received without backend");
                    }
                })
            });
        };
    }
    write_handler!(
        CreateEntity,
        |b: Arc<dyn crate::state::DynForgeBackend>, m: CreateEntity| async move {
            b.create(&m.entity).await
        },
        |v: &Entity, _: &CreateEntity, _: &Notification| Some(v.clone()),
        "created"
    );
    write_handler!(
        UpdateEntity,
        |b: Arc<dyn crate::state::DynForgeBackend>, m: UpdateEntity| async move {
            b.update(&m.entity).await
        },
        |v: &Entity, _: &UpdateEntity, _: &Notification| Some(v.clone()),
        "updated"
    );
    write_handler!(
        UpdateEntityIf,
        |b: Arc<dyn crate::state::DynForgeBackend>, m: UpdateEntityIf| async move {
            b.update_if(&m.entity, &m.expected).await
        },
        |v: &schema_forge_backend::conditional::VersionedEntity,
         _: &UpdateEntityIf,
         _: &Notification| Some(v.entity.clone()),
        "updated"
    );
    write_handler!(
        DeleteEntity,
        |b: Arc<dyn crate::state::DynForgeBackend>, m: DeleteEntity| async move {
            b.delete(&m.schema, &m.id).await
        },
        |_: &(), _: &DeleteEntity, n: &Notification| n.deleted.clone(),
        "deleted"
    );
    write_handler!(
        DeleteEntityIf,
        |b: Arc<dyn crate::state::DynForgeBackend>, m: DeleteEntityIf| async move {
            b.delete_if(&m.schema, &m.id, &m.expected).await
        },
        |_: &(), _: &DeleteEntityIf, n: &Notification| n.deleted.clone(),
        "deleted"
    );
    write_handler!(
        ProcessCreateIntent,
        |b: Arc<dyn crate::state::DynForgeBackend>, m: ProcessCreateIntent| async move {
            b.create_intent(&m.request).await
        },
        |v: &schema_forge_backend::create_intent::CreateIntentReceipt,
         m: &ProcessCreateIntent,
         _: &Notification| if v.created {
            match &m.request {
                schema_forge_backend::create_intent::CreateIntentRequest::Commit {
                    entity, ..
                } => Some(entity.clone()),
                _ => None,
            }
        } else {
            None
        },
        "created"
    );
}
