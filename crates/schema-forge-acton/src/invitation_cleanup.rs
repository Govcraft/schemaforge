//! Actor-owned invitation cleanup, including lifecycle-managed scheduling.
use std::result::Result;
use std::sync::Arc;

use acton_service::audit::{AuditLogger, AuditSeverity};
use acton_service::prelude::*;
use chrono::{DateTime, Utc};
use schema_forge_backend::invite_store::{InvitationPruneError, INVITATION_SWEEP_TIMEOUT};
use schema_forge_backend::{BackendError, InviteStore};

use crate::invites_config::InvitesConfig;
use crate::messages::ReplyChannel;

/// Initialize the invitation cleanup actor after provisioning the private store.
#[derive(Clone)]
pub struct InitializeInvitationCleanup {
    /// Validated deployment retention settings.
    pub config: InvitesConfig,
    /// Provisioned private invitation store.
    pub store: Arc<dyn InviteStore>,
    /// Existing service audit logger, when enabled.
    pub audit: Option<AuditLogger>,
    /// Completion barrier for startup wiring.
    pub reply: ReplyChannel<()>,
}
impl std::fmt::Debug for InitializeInvitationCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitializeInvitationCleanup")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Request one deterministic sweep. The optional reply is a completion barrier.
#[derive(Clone, Debug)]
pub struct SweepInvitations {
    /// Explicit time for deterministic callers, or current UTC time for timers.
    pub now: Option<DateTime<Utc>>,
    /// Optional completion barrier, including counts from partially completed sweeps.
    pub reply: Option<ReplyChannel<Result<u64, InvitationPruneError>>>,
}

/// Owns sweep state and its timer, and stops both during service shutdown.
#[derive(Default)]
pub struct InvitationCleanupActor {
    config: InvitesConfig,
    store: Option<Arc<dyn InviteStore>>,
    audit: Option<AuditLogger>,
    schedule: Option<ScheduledSend>,
}
impl std::fmt::Debug for InvitationCleanupActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvitationCleanupActor")
            .field("config", &self.config)
            .field("initialized", &self.store.is_some())
            .field("scheduled", &self.schedule.is_some())
            .finish()
    }
}
impl ActorExtension for InvitationCleanupActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        actor.mutate_on::<InitializeInvitationCleanup>(|actor, ctx| {
            let message = ctx.message().clone();
            if let Some(previous) = actor.model.schedule.take() {
                previous.cancel();
            }
            actor.model.config = message.config.clone();
            actor.model.store = Some(message.store);
            actor.model.audit = message.audit;
            if message.config.enabled() {
                if let Some(interval) = Interval::new(message.config.interval()) {
                    actor.model.schedule = Some(actor.handle().send_every(
                        SweepInvitations {
                            now: None,
                            reply: None,
                        },
                        interval,
                        Cadence::FixedDelay,
                    ));
                }
            }
            Reply::pending(async move {
                message.reply.send(()).await;
            })
        });
        actor.mutate_on::<SweepInvitations>(|actor, ctx| {
            let message = ctx.message().clone();
            let config = actor.model.config.clone();
            let store = actor.model.store.clone();
            let audit = actor.model.audit.clone();
            Reply::pending(sync_wrapper::SyncFuture::new(async move {
                let result = sweep(
                    &config,
                    store.as_deref(),
                    audit.as_ref(),
                    message.now.unwrap_or_else(Utc::now),
                    INVITATION_SWEEP_TIMEOUT + std::time::Duration::from_secs(1),
                )
                .await;
                if let Err(error) = &result {
                    tracing::warn!(%error, "invitation retention sweep failed");
                }
                if let Some(reply) = message.reply {
                    reply.send(result).await;
                }
            }))
        });
        actor.before_stop(|actor| {
            if let Some(schedule) = &actor.model.schedule {
                schedule.cancel();
            }
            Reply::ready()
        });
        actor.after_stop(|actor| {
            let schedule = actor.model.schedule.clone();
            Reply::pending(async move {
                if let Some(schedule) = schedule {
                    schedule.outcome().await;
                }
            })
        });
    }
}

async fn sweep(
    config: &InvitesConfig,
    store: Option<&dyn InviteStore>,
    audit: Option<&AuditLogger>,
    now: DateTime<Utc>,
    timeout: std::time::Duration,
) -> Result<u64, InvitationPruneError> {
    let before = config.cutoff(now).map_err(|error| {
        InvitationPruneError::before_removals(BackendError::Internal {
            message: error.to_string(),
        })
    })?;
    let (Some(before), Some(store)) = (before, store) else {
        return Ok(0);
    };
    // Production stores report committed pages before their deadline. Bound custom
    // store implementations too, so an unavailable store cannot hang shutdown.
    let result = tokio::time::timeout(timeout, store.prune(before))
        .await
        .unwrap_or_else(|_| {
            Err(InvitationPruneError::before_removals(
                BackendError::QueryError {
                    message: "invitation retention store exceeded its time budget".into(),
                },
            ))
        });
    let removed = match &result {
        Ok(removed) => *removed,
        Err(error) => error.removed,
    };
    if removed != 0 {
        if let Some(audit) = audit {
            audit
                .log_custom(
                    "forge.invite.pruned",
                    AuditSeverity::Notice,
                    Some(serde_json::json!({"count": removed})),
                )
                .await;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use acton_service::audit::{AuditConfig, AuditEvent, AuditEventKind};
    use schema_forge_backend::{ForgeInvitation, NewInvitation};
    use schema_forge_core::types::EntityId;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    #[derive(Default)]
    struct Store {
        calls: AtomicUsize,
        removed: AtomicU64,
        fail: AtomicBool,
        stall: bool,
    }
    #[async_trait::async_trait]
    impl InviteStore for Store {
        async fn prune(&self, _: DateTime<Utc>) -> Result<u64, InvitationPruneError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.stall {
                std::future::pending::<()>().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(InvitationPruneError {
                    removed: 7,
                    source: BackendError::Internal {
                        message: "synthetic later-page outage".into(),
                    },
                });
            }
            Ok(self.removed.swap(0, Ordering::SeqCst))
        }
        async fn create(&self, _: NewInvitation) -> Result<ForgeInvitation, BackendError> {
            panic!("cleanup cannot create invitations")
        }
        async fn find_by_jti(&self, _: &str) -> Result<Option<ForgeInvitation>, BackendError> {
            panic!("cleanup cannot inspect individual invitations")
        }
        async fn mark_consumed(&self, _: &EntityId, _: DateTime<Utc>) -> Result<(), BackendError> {
            panic!("cleanup cannot consume invitations")
        }
    }
    #[derive(Default, Debug)]
    struct AuditCollector {
        events: Vec<AuditEvent>,
    }
    #[derive(Clone, Debug)]
    struct ReadAudit {
        reply: ReplyChannel<Vec<AuditEvent>>,
    }
    #[derive(Clone, Debug)]
    struct ReadSchedule {
        reply: ReplyChannel<Option<ScheduledSend>>,
    }

    async fn read_schedule(handle: &ActorHandle) -> Option<ScheduledSend> {
        let (tx, rx) = oneshot::channel();
        handle
            .send(ReadSchedule {
                reply: ReplyChannel::new(tx),
            })
            .await;
        rx.await.unwrap()
    }
    async fn read_audit(handle: &ActorHandle) -> Vec<AuditEvent> {
        let (tx, rx) = oneshot::channel();
        handle
            .send(ReadAudit {
                reply: ReplyChannel::new(tx),
            })
            .await;
        rx.await.unwrap()
    }
    async fn initialize(
        handle: &ActorHandle,
        store: Arc<Store>,
        config: InvitesConfig,
        audit: Option<AuditLogger>,
    ) {
        let (tx, rx) = oneshot::channel();
        handle
            .send(InitializeInvitationCleanup {
                config,
                store,
                audit,
                reply: ReplyChannel::new(tx),
            })
            .await;
        rx.await.unwrap();
    }
    async fn run(handle: &ActorHandle) -> Result<u64, InvitationPruneError> {
        let (tx, rx) = oneshot::channel();
        handle
            .send(SweepInvitations {
                now: None,
                reply: Some(ReplyChannel::new(tx)),
            })
            .await;
        rx.await.unwrap()
    }
    async fn runtime() -> (ActorRuntime, ActorHandle) {
        let mut runtime = ActonApp::launch_async().await;
        let mut actor = runtime.new_actor::<InvitationCleanupActor>();
        InvitationCleanupActor::configure(&mut actor);
        actor.act_on::<ReadSchedule>(|actor, ctx| {
            let schedule = actor.model.schedule.clone();
            let reply = ctx.message().reply.clone();
            Reply::pending(async move {
                reply.send(schedule).await;
            })
        });
        let handle = actor.start().await;
        (runtime, handle)
    }
    #[tokio::test]
    async fn a_stalled_custom_store_is_bounded() {
        let store = Store {
            stall: true,
            ..Default::default()
        };
        let error = sweep(
            &InvitesConfig::default(),
            Some(&store),
            None,
            Utc::now(),
            std::time::Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert_eq!(error.removed, 0);
        assert!(error.source.to_string().contains("time budget"));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabled_retention_never_schedules_or_touches_storage() {
        let (mut runtime, handle) = runtime().await;
        let store = Arc::new(Store::default());
        let disabled = serde_json::from_str(r#"{"retention_days":0}"#).unwrap();
        initialize(&handle, store.clone(), disabled, None).await;
        assert!(read_schedule(&handle).await.is_none());
        assert_eq!(run(&handle).await.unwrap(), 0);
        assert_eq!(store.calls.load(Ordering::SeqCst), 0);
        runtime.shutdown_all().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sweeps_emit_one_aggregate_audit_event_and_stop_the_timer() {
        let (mut runtime, handle) = runtime().await;
        let mut collector = runtime.new_actor::<AuditCollector>();
        collector.mutate_on::<AuditEvent>(|actor, ctx| {
            actor.model.events.push(ctx.message().clone());
            Reply::ready()
        });
        collector.act_on::<ReadAudit>(|actor, ctx| {
            let events = actor.model.events.clone();
            let reply = ctx.message().reply.clone();
            Reply::pending(async move {
                reply.send(events).await;
            })
        });
        let audit_handle = collector.start().await;
        let logger = AuditLogger::new(
            audit_handle.clone(),
            "schemaforge".into(),
            AuditConfig::default(),
        );
        let store = Arc::new(Store::default());
        store.removed.store(3, Ordering::SeqCst);
        initialize(
            &handle,
            store.clone(),
            InvitesConfig::default(),
            Some(logger),
        )
        .await;
        let schedule = read_schedule(&handle).await.unwrap();
        assert!(!schedule.is_settled());
        assert_eq!(run(&handle).await.unwrap(), 3);
        assert_eq!(run(&handle).await.unwrap(), 0);
        store.fail.store(true, Ordering::SeqCst);
        assert!(run(&handle).await.is_err());
        let events = read_audit(&audit_handle).await;
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].kind,
            AuditEventKind::Custom("forge.invite.pruned".into())
        );
        assert_eq!(events[0].metadata, Some(serde_json::json!({"count": 3})));
        assert!(events[0].source.subject.is_none());
        assert_eq!(
            events[1].metadata,
            Some(serde_json::json!({"count": 7})),
            "a partially committed sweep must still produce one aggregate event"
        );
        store.fail.store(false, Ordering::SeqCst);
        store.removed.store(2, Ordering::SeqCst);
        assert_eq!(
            run(&handle).await.unwrap(),
            2,
            "later sweeps recover after a storage failure"
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), runtime.shutdown_all())
            .await
            .unwrap()
            .unwrap();
        assert!(
            schedule.is_settled(),
            "shutdown must join the scheduled-send timer"
        );
        assert_eq!(schedule.deliveries(), 0);
    }
}
