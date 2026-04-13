//! Lifecycle hook dispatch.
//!
//! This module wires `@hook(event)` declarations in the schema DSL to
//! external services that SchemaForge consults during entity CRUD
//! operations. Each declared hook maps to a [`HookBinding`] in
//! configuration: a schema name, a lifecycle event, an endpoint URL,
//! and policy (timeout, `required`).
//!
//! Two trait implementations are provided:
//!
//! * [`MockHookDispatcher`] — records invocations and returns caller-
//!   supplied responses. Used in tests and for `hooks.enabled = false`
//!   deployments that still exercise the control-flow plumbing.
//! * [`tonic_dispatcher::TonicHookDispatcher`] — real gRPC dispatcher
//!   backed by tonic + `prost-reflect`. Loads `FileDescriptorSet`
//!   binaries on construction and dynamically encodes/decodes typed
//!   per-schema messages at call time.
//!
//! The route-level contract is:
//!
//! 1. **Blocking (`before_*`)**: the handler awaits the dispatcher
//!    response before persisting. If `abort_reason` is set, the request
//!    aborts with [`crate::error::ForgeError::HookAborted`]. If the
//!    response carries modified fields, they replace the entity payload.
//! 2. **Fire-and-forget (`after_*`)**: the dispatcher spawns the call on
//!    a background task and returns immediately. Errors are logged.
//!
//! See `docs/hooks.md` for full semantics.

pub mod tonic_dispatcher;
pub use tonic_dispatcher::{TonicDispatcherConfig, TonicHookDispatcher};

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use schema_forge_core::types::{DynamicValue, HookEvent};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

// ---------------------------------------------------------------------------
// HooksConfig
// ---------------------------------------------------------------------------

/// Top-level hook configuration, read from the `[schema_forge.hooks]`
/// section of `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Whether hook dispatch is enabled. Default: `false`.
    ///
    /// When `false`, all hook annotations in schemas are ignored at runtime —
    /// useful for local development and for environments where hook services
    /// are not yet deployed.
    #[serde(default)]
    pub enabled: bool,

    /// Default timeout per blocking hook call, in milliseconds. Default: 5000.
    #[serde(default = "default_timeout_ms")]
    pub default_timeout_ms: u32,

    /// Default maximum concurrent fire-and-forget (`after_*`) dispatches.
    /// Default: 100.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_async: usize,

    /// Per-hook bindings. Each entry is a `(schema, event)` pair bound to
    /// an endpoint and policy.
    #[serde(default)]
    pub bindings: Vec<HookBinding>,
}

fn default_timeout_ms() -> u32 {
    5000
}

fn default_max_concurrent() -> usize {
    100
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_timeout_ms: default_timeout_ms(),
            max_concurrent_async: default_max_concurrent(),
            bindings: Vec::new(),
        }
    }
}

impl HooksConfig {
    /// Look up a binding by schema name and event.
    pub fn binding_for(&self, schema: &str, event: HookEvent) -> Option<&HookBinding> {
        self.bindings
            .iter()
            .find(|b| b.schema == schema && b.event == event)
    }
}

/// A single `(schema, event)` → endpoint binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookBinding {
    /// Schema name (PascalCase) that this binding applies to.
    pub schema: String,

    /// Lifecycle event that triggers the dispatch.
    pub event: HookEvent,

    /// gRPC endpoint URL (e.g. `http://translation-hook:9090`).
    pub endpoint: String,

    /// Per-binding timeout override. Falls back to `default_timeout_ms`.
    #[serde(default)]
    pub timeout_ms: Option<u32>,

    /// If `true`, the request fails when the hook is unavailable or times
    /// out. If `false` (default), the operation proceeds and the failure
    /// is logged.
    #[serde(default)]
    pub required: bool,

    /// Path to the compiled `FileDescriptorSet` binary produced by the
    /// scaffold. Used by the tonic dispatcher in Phase 2b to locate the
    /// typed request/response messages. Ignored by [`MockHookDispatcher`].
    #[serde(default)]
    pub descriptor_path: Option<String>,
}

impl HookBinding {
    /// Effective timeout, honoring the per-binding override.
    pub fn effective_timeout_ms(&self, config: &HooksConfig) -> u32 {
        self.timeout_ms.unwrap_or(config.default_timeout_ms)
    }
}

// ---------------------------------------------------------------------------
// HookInvocation / HookOutcome
// ---------------------------------------------------------------------------

/// Payload passed to a dispatcher call. Represents a point-in-time snapshot
/// of the entity state at a given lifecycle event.
#[derive(Debug, Clone)]
pub struct HookInvocation {
    /// Schema name (PascalCase).
    pub schema: String,
    /// Lifecycle event.
    pub event: HookEvent,
    /// Operation context (`"create"`, `"update"`, `"delete"`, `"read"`, ...).
    pub operation: String,
    /// Authenticated user id (from claims), if any.
    pub user_id: Option<String>,
    /// Entity id, if known (`None` for `before_validate` / `before_change` on
    /// create, where no id has been assigned yet).
    pub entity_id: Option<String>,
    /// Entity field snapshot at the moment of dispatch.
    pub fields: BTreeMap<String, DynamicValue>,
}

/// Outcome of a blocking (`before_*`) dispatch.
///
/// Fire-and-forget (`after_*`) dispatches do not produce a `HookOutcome` —
/// they are acknowledged via a separate `Result<(), HookError>`.
#[derive(Debug, Clone, Default)]
pub struct HookOutcome {
    /// If `Some`, the request is aborted with this message and no persistence
    /// occurs. The reason is surfaced to the client as a 422.
    pub abort_reason: Option<String>,
    /// If `Some`, these fields replace the entity payload before persistence
    /// (or, for `after_read`, before serialization to the response). Fields
    /// not present in the map are left untouched.
    pub modified_fields: Option<BTreeMap<String, DynamicValue>>,
}

// ---------------------------------------------------------------------------
// HookError
// ---------------------------------------------------------------------------

/// Errors produced by a dispatcher.
#[derive(Debug, Clone)]
pub enum HookError {
    /// The request completed but the hook returned an abort reason.
    Aborted(String),
    /// The hook did not respond within its timeout budget.
    Timeout { endpoint: String, timeout_ms: u32 },
    /// The endpoint is unreachable or returned a transport error.
    Unavailable { endpoint: String, message: String },
    /// The hook response could not be decoded or violated the contract.
    Protocol { message: String },
    /// Internal dispatcher error (configuration mismatch, descriptor drift,
    /// etc.).
    Internal { message: String },
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aborted(reason) => write!(f, "hook aborted: {reason}"),
            Self::Timeout {
                endpoint,
                timeout_ms,
            } => write!(f, "hook at {endpoint} timed out after {timeout_ms}ms"),
            Self::Unavailable { endpoint, message } => {
                write!(f, "hook at {endpoint} unavailable: {message}")
            }
            Self::Protocol { message } => write!(f, "hook protocol error: {message}"),
            Self::Internal { message } => write!(f, "hook dispatcher error: {message}"),
        }
    }
}

impl std::error::Error for HookError {}

// ---------------------------------------------------------------------------
// HookDispatcher trait
// ---------------------------------------------------------------------------

/// Abstraction over hook transport. Phase 2a ships a mock implementation;
/// Phase 2b adds a real tonic + prost-reflect implementation.
#[async_trait]
pub trait HookDispatcher: Send + Sync + std::fmt::Debug {
    /// Dispatch a blocking (`before_*`) hook. The caller awaits the outcome
    /// before continuing with persistence.
    async fn call_before(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<HookOutcome, HookError>;

    /// Dispatch a fire-and-forget (`after_*`) hook. The implementation is
    /// responsible for spawning its own background task — the caller will
    /// await this function and expects it to return promptly.
    async fn call_after(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<(), HookError>;
}

// ---------------------------------------------------------------------------
// run_before_hook helper
// ---------------------------------------------------------------------------

/// Run a `before_*` hook for the given schema and event, applying the
/// binding's `required` policy to timeout/unavailable errors.
///
/// Returns:
/// * `Ok(Some(outcome))` — the hook ran and may have modified fields or
///   aborted.
/// * `Ok(None)` — no binding configured for this (schema, event) pair, or
///   hooks are globally disabled. The caller should proceed with unmodified
///   data.
/// * `Err(HookError::Aborted)` — the hook explicitly aborted the request.
/// * `Err(...)` — required hook failed. Non-required failures are absorbed
///   and return `Ok(None)` with a warning logged.
pub async fn run_before_hook(
    dispatcher: &dyn HookDispatcher,
    config: &HooksConfig,
    invocation: HookInvocation,
) -> Result<Option<HookOutcome>, HookError> {
    if !config.enabled {
        return Ok(None);
    }
    let Some(binding) = config.binding_for(&invocation.schema, invocation.event) else {
        return Ok(None);
    };

    debug!(
        schema = %invocation.schema,
        event = ?invocation.event,
        endpoint = %binding.endpoint,
        required = binding.required,
        "dispatching before hook"
    );

    match dispatcher.call_before(binding, invocation).await {
        Ok(outcome) => {
            if let Some(reason) = &outcome.abort_reason {
                // Always propagate explicit aborts regardless of `required`.
                return Err(HookError::Aborted(reason.clone()));
            }
            Ok(Some(outcome))
        }
        Err(e) if binding.required => Err(e),
        Err(e) => {
            warn!(
                schema = ?binding.schema,
                event = ?binding.event,
                endpoint = %binding.endpoint,
                error = %e,
                "non-required before hook failed, proceeding"
            );
            Ok(None)
        }
    }
}

/// Run an `after_*` hook as fire-and-forget. Never blocks the caller's
/// response path; dispatch errors are logged.
pub async fn run_after_hook(
    dispatcher: &dyn HookDispatcher,
    config: &HooksConfig,
    invocation: HookInvocation,
) {
    if !config.enabled {
        return;
    }
    let Some(binding) = config.binding_for(&invocation.schema, invocation.event) else {
        return;
    };
    debug!(
        schema = %invocation.schema,
        event = ?invocation.event,
        endpoint = %binding.endpoint,
        "dispatching after hook"
    );
    if let Err(e) = dispatcher.call_after(binding, invocation).await {
        error!(
            schema = ?binding.schema,
            event = ?binding.event,
            endpoint = %binding.endpoint,
            error = %e,
            "after hook dispatch failed"
        );
    }
}

// ---------------------------------------------------------------------------
// MockHookDispatcher
// ---------------------------------------------------------------------------

/// Test dispatcher that records every invocation and replays caller-
/// configured outcomes. Also handles simulated unavailability so route-
/// interception tests can exercise `required` / non-required paths without
/// standing up a real tonic server.
#[derive(Debug, Clone, Default)]
pub struct MockHookDispatcher {
    inner: Arc<tokio::sync::Mutex<MockState>>,
}

#[derive(Debug, Default)]
struct MockState {
    /// Invocations captured in call order.
    before_calls: Vec<HookInvocation>,
    after_calls: Vec<HookInvocation>,
    /// Map `(schema, event)` → canned outcome for `before_*` calls.
    before_responses: BTreeMap<(String, HookEvent), MockBeforeResponse>,
    /// Map `(schema, event)` → canned response for `after_*` calls.
    after_responses: BTreeMap<(String, HookEvent), MockAfterResponse>,
}

#[derive(Debug, Clone)]
enum MockBeforeResponse {
    Outcome(HookOutcome),
    Error(HookError),
}

#[derive(Debug, Clone)]
enum MockAfterResponse {
    Error(HookError),
}

impl MockHookDispatcher {
    /// Create a fresh mock dispatcher with no canned responses.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure a canned outcome for a `before_*` call.
    pub async fn respond_before(
        &self,
        schema: impl Into<String>,
        event: HookEvent,
        outcome: HookOutcome,
    ) {
        self.inner
            .lock()
            .await
            .before_responses
            .insert((schema.into(), event), MockBeforeResponse::Outcome(outcome));
    }

    /// Configure a canned error for a `before_*` call.
    pub async fn fail_before(&self, schema: impl Into<String>, event: HookEvent, error: HookError) {
        self.inner
            .lock()
            .await
            .before_responses
            .insert((schema.into(), event), MockBeforeResponse::Error(error));
    }

    /// Configure an `after_*` call to fail.
    pub async fn fail_after(&self, schema: impl Into<String>, event: HookEvent, error: HookError) {
        self.inner
            .lock()
            .await
            .after_responses
            .insert((schema.into(), event), MockAfterResponse::Error(error));
    }

    /// Snapshot the captured `before_*` invocations, in call order.
    pub async fn before_calls(&self) -> Vec<HookInvocation> {
        self.inner.lock().await.before_calls.clone()
    }

    /// Snapshot the captured `after_*` invocations, in call order.
    pub async fn after_calls(&self) -> Vec<HookInvocation> {
        self.inner.lock().await.after_calls.clone()
    }
}

#[async_trait]
impl HookDispatcher for MockHookDispatcher {
    async fn call_before(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<HookOutcome, HookError> {
        let mut state = self.inner.lock().await;
        let key = (binding.schema.clone(), binding.event);
        state.before_calls.push(invocation.clone());
        match state.before_responses.get(&key).cloned() {
            Some(MockBeforeResponse::Outcome(o)) => Ok(o),
            Some(MockBeforeResponse::Error(e)) => Err(e),
            None => Ok(HookOutcome::default()),
        }
    }

    async fn call_after(
        &self,
        binding: &HookBinding,
        invocation: HookInvocation,
    ) -> Result<(), HookError> {
        let mut state = self.inner.lock().await;
        let key = (binding.schema.clone(), binding.event);
        state.after_calls.push(invocation);
        match state.after_responses.get(&key).cloned() {
            None => Ok(()),
            Some(MockAfterResponse::Error(e)) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(schema: &str, event: HookEvent) -> HookInvocation {
        HookInvocation {
            schema: schema.to_string(),
            event,
            operation: "create".to_string(),
            user_id: None,
            entity_id: None,
            fields: BTreeMap::new(),
        }
    }

    fn cfg(bindings: Vec<HookBinding>) -> HooksConfig {
        HooksConfig {
            enabled: true,
            bindings,
            ..HooksConfig::default()
        }
    }

    fn binding(schema: &str, event: HookEvent, required: bool) -> HookBinding {
        HookBinding {
            schema: schema.to_string(),
            event,
            endpoint: "http://test".to_string(),
            timeout_ms: None,
            required,
            descriptor_path: None,
        }
    }

    #[tokio::test]
    async fn run_before_hook_skipped_when_disabled() {
        let dispatcher = MockHookDispatcher::new();
        let config = HooksConfig::default(); // enabled=false
        let result = run_before_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::BeforeChange),
        )
        .await
        .unwrap();
        assert!(result.is_none());
        assert!(dispatcher.before_calls().await.is_empty());
    }

    #[tokio::test]
    async fn run_before_hook_skipped_when_no_binding() {
        let dispatcher = MockHookDispatcher::new();
        let config = cfg(vec![]);
        let result = run_before_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::BeforeChange),
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn run_before_hook_returns_outcome() {
        let dispatcher = MockHookDispatcher::new();
        let mut modified = BTreeMap::new();
        modified.insert("name".to_string(), DynamicValue::Text("patched".into()));
        dispatcher
            .respond_before(
                "S",
                HookEvent::BeforeChange,
                HookOutcome {
                    abort_reason: None,
                    modified_fields: Some(modified.clone()),
                },
            )
            .await;
        let config = cfg(vec![binding("S", HookEvent::BeforeChange, true)]);
        let outcome = run_before_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::BeforeChange),
        )
        .await
        .unwrap()
        .expect("outcome should be present");
        assert_eq!(outcome.modified_fields.as_ref().unwrap(), &modified);
    }

    #[tokio::test]
    async fn run_before_hook_propagates_abort() {
        let dispatcher = MockHookDispatcher::new();
        dispatcher
            .respond_before(
                "S",
                HookEvent::BeforeChange,
                HookOutcome {
                    abort_reason: Some("nope".into()),
                    modified_fields: None,
                },
            )
            .await;
        let config = cfg(vec![binding("S", HookEvent::BeforeChange, false)]);
        let err = run_before_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::BeforeChange),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, HookError::Aborted(ref r) if r == "nope"));
    }

    #[tokio::test]
    async fn run_before_hook_required_error_propagates() {
        let dispatcher = MockHookDispatcher::new();
        dispatcher
            .fail_before(
                "S",
                HookEvent::BeforeChange,
                HookError::Unavailable {
                    endpoint: "http://test".into(),
                    message: "connection refused".into(),
                },
            )
            .await;
        let config = cfg(vec![binding("S", HookEvent::BeforeChange, true)]);
        let err = run_before_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::BeforeChange),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, HookError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn run_before_hook_optional_error_absorbed() {
        let dispatcher = MockHookDispatcher::new();
        dispatcher
            .fail_before(
                "S",
                HookEvent::BeforeChange,
                HookError::Unavailable {
                    endpoint: "http://test".into(),
                    message: "connection refused".into(),
                },
            )
            .await;
        let config = cfg(vec![binding("S", HookEvent::BeforeChange, false)]);
        let result = run_before_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::BeforeChange),
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn run_after_hook_records_invocation() {
        let dispatcher = MockHookDispatcher::new();
        let config = cfg(vec![binding("S", HookEvent::AfterChange, false)]);
        run_after_hook(
            &dispatcher,
            &config,
            invocation("S", HookEvent::AfterChange),
        )
        .await;
        assert_eq!(dispatcher.after_calls().await.len(), 1);
    }

    #[tokio::test]
    async fn hooks_config_defaults() {
        let config = HooksConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.default_timeout_ms, 5000);
        assert_eq!(config.max_concurrent_async, 100);
        assert!(config.bindings.is_empty());
    }

    #[tokio::test]
    async fn binding_effective_timeout_respects_override() {
        let mut b = binding("S", HookEvent::BeforeChange, false);
        b.timeout_ms = Some(1234);
        let config = HooksConfig::default();
        assert_eq!(b.effective_timeout_ms(&config), 1234);
        b.timeout_ms = None;
        assert_eq!(b.effective_timeout_ms(&config), 5000);
    }
}
