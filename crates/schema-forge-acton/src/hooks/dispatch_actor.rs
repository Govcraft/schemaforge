//! Actor that runs `after_*` hook dispatches.
//!
//! Issue #11 background: prior to v0.13, the route handler for entity
//! mutations called `tokio::spawn` directly to fire the post-commit
//! `after_*` hook. This worked but was an anti-pattern for a production
//! deployment: the spawned task lived outside acton-reactive's
//! supervision tree, so it had no graceful-shutdown coordination, no
//! backpressure, and no way for the runtime to drain in-flight hook
//! dispatches when the service is asked to stop.
//!
//! The fix is a tiny dedicated actor whose only job is to receive a
//! [`DispatchHook`] message and run [`run_after_hook`] under acton's
//! scheduler. The actor uses `act_on` (read-only state, concurrent
//! handler execution). Configured semaphore admission bounds active calls;
//! the runtime also bounds batches and buffers queued messages. A slow batch
//! can apply backpressure to subsequent dispatches.
//!
//! Per-schema or per-endpoint pooling is a potential future
//! optimization; one shared actor is sufficient for the current
//! workload and removes the deadlock entirely because the actor never
//! holds a lock on the trigger entity row.

use std::sync::{Arc, OnceLock};
use tokio::sync::{oneshot, Mutex, Semaphore};

use acton_service::prelude::*;
use sync_wrapper::SyncFuture;
use tracing::warn;

use super::{run_after_hook, HookDispatcher, HookInvocation, HooksConfig};

// ---------------------------------------------------------------------------
// HookDispatchActor
// ---------------------------------------------------------------------------

/// Actor extension for post-commit `after_*` hook dispatch.
///
/// Stateless from the runtime's perspective: every required input
/// (dispatcher handle, hooks config, invocation payload) travels with
/// the [`DispatchHook`] message, so handlers can run concurrently with
/// no shared mutable state.
#[derive(Default, Debug)]
pub struct HookDispatchActor;

// Restart policy is left at the default (`Permanent`), which acton-service
// 0.35.0 made effective for the first time. That is the right policy here
// precisely because this actor is stateless: every input travels with the
// message, so a rebuilt-from-`Default` replacement is indistinguishable from
// the original. Contrast `ForgeActor`, whose whole state arrives in one
// `InitForge` at boot and which therefore opts out.
impl ActorExtension for HookDispatchActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        // Configuration is immutable for an actor incarnation. Keep the unit
        // actor's public construction compatible and initialize admission from
        // the first dispatch, including after an actor restart.
        let limit = Arc::new(OnceLock::<Arc<Semaphore>>::new());
        let deferred_limit = limit.clone();
        actor.act_on::<DispatchHook>(move |_actor, ctx| {
            let msg = ctx.message().clone();
            let semaphore = limit
                .get_or_init(|| {
                    Arc::new(Semaphore::new(
                        msg.config
                            .max_concurrent_async
                            .clamp(1, Semaphore::MAX_PERMITS),
                    ))
                })
                .clone();
            Reply::pending(SyncFuture::new(run_dispatch(msg, semaphore)))
        });
        actor.act_on::<DeferredHook>(move |_actor, ctx| {
            let message = ctx.message().clone();
            let semaphore = deferred_limit
                .get_or_init(|| {
                    Arc::new(Semaphore::new(
                        message
                            .config
                            .max_concurrent_async
                            .clamp(1, Semaphore::MAX_PERMITS),
                    ))
                })
                .clone();
            Reply::pending(SyncFuture::new(async move {
                let receiver = message.receiver.lock().await.take();
                if let Some(receiver) = receiver {
                    if let Ok(dispatch) = receiver.await {
                        run_dispatch(dispatch, semaphore).await;
                    }
                }
            }))
        });
    }
}

async fn run_dispatch(message: DispatchHook, semaphore: Arc<Semaphore>) {
    let Some(dispatcher) = message.dispatcher else {
        warn!(schema = %message.invocation.schema, "hook dispatch received without a dispatcher");
        return;
    };
    let Ok(_permit) = semaphore.acquire().await else {
        warn!("after-hook admission closed");
        return;
    };
    run_after_hook(dispatcher.as_ref(), &message.config, message.invocation).await;
}

/// Reserve hook queue admission before submitting a mutation. The actor waits
/// for a commit payload, or exits if validation/submission/storage fails.
/// The receiver has one owner; the wrapper only satisfies message cloning.
#[derive(Clone, Debug)]
pub(crate) struct DeferredHook {
    pub config: HooksConfig,
    receiver: Arc<Mutex<Option<oneshot::Receiver<DispatchHook>>>>,
}
impl DeferredHook {
    pub(crate) fn new(config: HooksConfig, receiver: oneshot::Receiver<DispatchHook>) -> Self {
        Self {
            config,
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }
}

// ---------------------------------------------------------------------------
// DispatchHook message
// ---------------------------------------------------------------------------

/// Fire-and-forget message asking the actor to run an `after_*` hook.
///
/// The dispatcher and config are bundled with the message so the actor
/// never has to consult its own state — this is what makes `act_on`
/// (read-only, concurrent) the right handler choice and removes any
/// possibility of self-deadlock with the trigger entity's persistence
/// path.
#[derive(Clone)]
pub struct DispatchHook {
    /// The hook payload to dispatch. Already contains schema, event,
    /// operation, user id, entity id, and the field snapshot.
    pub invocation: HookInvocation,
    /// The dispatcher to use. Wrapped in `Option` so the actor can warn
    /// (rather than panic) if it is somehow asked to run before the
    /// route layer has wired hooks up.
    pub dispatcher: Option<Arc<dyn HookDispatcher>>,
    /// Snapshot of the hooks config at the time the route handler
    /// queued the message. Cloned so the dispatch doesn't race with
    /// later config reloads.
    pub config: HooksConfig,
}

impl std::fmt::Debug for DispatchHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchHook")
            .field("schema", &self.invocation.schema)
            .field("event", &self.invocation.event)
            .field("operation", &self.invocation.operation)
            .field("entity_id", &self.invocation.entity_id)
            .field("dispatcher", &self.dispatcher.as_ref().map(|_| ".."))
            .finish()
    }
}
