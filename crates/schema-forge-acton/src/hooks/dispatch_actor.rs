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
//! handler execution) so a slow hook endpoint can't block other
//! dispatches — the runtime's high-water mark is the natural
//! backpressure mechanism.
//!
//! Per-schema or per-endpoint pooling is a potential future
//! optimization; one shared actor is sufficient for the current
//! workload and removes the deadlock entirely because the actor never
//! holds a lock on the trigger entity row.

use std::sync::Arc;

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

impl ActorExtension for HookDispatchActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        actor.act_on::<DispatchHook>(|_actor, ctx| {
            let msg = ctx.message().clone();
            // The acton-reactive `FutureBox` requires `Send + Sync`,
            // but the `HookDispatcher` trait returns `BoxFuture<'_, T>`
            // which is `Send` only (the `async_trait` default). Wrap
            // the async block in `SyncFuture` to satisfy the bound;
            // this is sound because `Future::poll` already requires
            // exclusive access to the future via `Pin<&mut Self>`.
            Reply::pending(SyncFuture::new(async move {
                let Some(dispatcher) = msg.dispatcher else {
                    warn!(
                        schema = %msg.invocation.schema,
                        event = ?msg.invocation.event,
                        "DispatchHook received but no dispatcher is configured"
                    );
                    return;
                };
                run_after_hook(dispatcher.as_ref(), &msg.config, msg.invocation).await;
            }))
        });
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
