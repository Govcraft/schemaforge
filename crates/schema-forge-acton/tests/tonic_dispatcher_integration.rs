//! Integration test for the real tonic + prost-reflect [`TonicHookDispatcher`].
//!
//! Spins up an in-process tonic server backed by codegen from
//! `tests/proto/translation_hooks.proto`, points the dispatcher at it, and
//! exercises the same modify / abort / fire-and-forget paths covered by
//! `hooks_integration.rs` — but through the real wire format rather than
//! the in-memory `MockHookDispatcher`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use schema_forge_acton::hooks::{
    HookBinding, HookDispatcher, HookError, HookInvocation, HooksConfig, TonicDispatcherConfig,
    TonicHookDispatcher,
};
use schema_forge_core::types::{DynamicValue, HookEvent};
use tokio::sync::oneshot;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

mod pb {
    include!(concat!(env!("OUT_DIR"), "/schema_forge_test.rs"));
}

use pb::translation_hooks_server::{TranslationHooks, TranslationHooksServer};
use pb::{
    TranslationAfterChangeRequest, TranslationAfterChangeResponse, TranslationBeforeChangeRequest,
    TranslationBeforeChangeResponse,
};

const DESCRIPTOR_PATH: &str = env!("TRANSLATION_HOOKS_DESCRIPTOR");

#[derive(Default)]
struct CapturedRequest {
    source_text: String,
    operation: String,
    user_id: Option<String>,
    entity_id: Option<String>,
}

struct TestService {
    behavior: Behavior,
    captured: tokio::sync::Mutex<Vec<CapturedRequest>>,
    after_count: tokio::sync::Mutex<usize>,
}

#[derive(Clone)]
enum Behavior {
    /// Echo back source_text and patch translated_text with a fixed string.
    Modify,
    /// Return abort_reason.
    Abort(String),
    /// Pass-through (no modifications).
    PassThrough,
}

#[tonic::async_trait]
impl TranslationHooks for TestService {
    async fn before_change(
        &self,
        request: Request<TranslationBeforeChangeRequest>,
    ) -> Result<Response<TranslationBeforeChangeResponse>, Status> {
        let req = request.into_inner();
        self.captured.lock().await.push(CapturedRequest {
            source_text: req.source_text.clone(),
            operation: req.operation.clone(),
            user_id: req.user_id.clone(),
            entity_id: req.entity_id.clone(),
        });
        let resp = match &self.behavior {
            Behavior::Modify => TranslationBeforeChangeResponse {
                abort_reason: None,
                source_text: None,
                translated_text: Some("¡hola!".to_string()),
            },
            Behavior::Abort(reason) => TranslationBeforeChangeResponse {
                abort_reason: Some(reason.clone()),
                source_text: None,
                translated_text: None,
            },
            Behavior::PassThrough => TranslationBeforeChangeResponse::default(),
        };
        Ok(Response::new(resp))
    }

    async fn after_change(
        &self,
        _request: Request<TranslationAfterChangeRequest>,
    ) -> Result<Response<TranslationAfterChangeResponse>, Status> {
        *self.after_count.lock().await += 1;
        Ok(Response::new(TranslationAfterChangeResponse::default()))
    }
}

/// Spawn a tonic server on a random port; return the bound address and a
/// shutdown trigger. The server holds a reference to the captured-request
/// vec so the test can inspect what arrived.
async fn spawn_server(behavior: Behavior) -> (SocketAddr, Arc<TestService>, oneshot::Sender<()>) {
    let svc = Arc::new(TestService {
        behavior,
        captured: tokio::sync::Mutex::new(Vec::new()),
        after_count: tokio::sync::Mutex::new(0),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let svc_clone = Arc::clone(&svc);

    // The generated server only accepts an owned `T: TranslationHooks`, so
    // wrap with a small adapter that holds the Arc.
    struct AdapterSvc(Arc<TestService>);
    #[tonic::async_trait]
    impl TranslationHooks for AdapterSvc {
        async fn before_change(
            &self,
            request: Request<TranslationBeforeChangeRequest>,
        ) -> Result<Response<TranslationBeforeChangeResponse>, Status> {
            self.0.before_change(request).await
        }
        async fn after_change(
            &self,
            request: Request<TranslationAfterChangeRequest>,
        ) -> Result<Response<TranslationAfterChangeResponse>, Status> {
            self.0.after_change(request).await
        }
    }

    tokio::spawn(async move {
        Server::builder()
            .add_service(TranslationHooksServer::new(AdapterSvc(svc_clone)))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    // Tiny startup race buffer.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, svc, shutdown_tx)
}

fn binding(addr: SocketAddr, event: HookEvent) -> HookBinding {
    HookBinding {
        schema: "Translation".to_string(),
        event,
        endpoint: format!("http://{addr}"),
        timeout_ms: Some(2000),
        required: true,
        descriptor_path: Some(DESCRIPTOR_PATH.to_string()),
    }
}

fn invocation(event: HookEvent) -> HookInvocation {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "source_text".to_string(),
        DynamicValue::Text("hello".to_string()),
    );
    fields.insert(
        "translated_text".to_string(),
        DynamicValue::Text(String::new()),
    );
    HookInvocation {
        schema: "Translation".to_string(),
        event,
        operation: "create".to_string(),
        user_id: Some("user:test".to_string()),
        entity_id: None,
        fields,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_change_modifies_field() {
    let (addr, svc, shutdown) = spawn_server(Behavior::Modify).await;
    let cfg = HooksConfig {
        enabled: true,
        bindings: vec![binding(addr, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let dispatcher =
        TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).expect("dispatcher");
    let outcome = dispatcher
        .call_before(&cfg.bindings[0], invocation(HookEvent::BeforeChange))
        .await
        .expect("call");
    let modified = outcome.modified_fields.expect("modifications present");
    assert_eq!(
        modified.get("translated_text"),
        Some(&DynamicValue::Text("¡hola!".to_string()))
    );

    let captured = svc.captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].source_text, "hello");
    assert_eq!(captured[0].operation, "create");
    assert_eq!(captured[0].user_id.as_deref(), Some("user:test"));
    assert!(captured[0].entity_id.is_none());

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_change_abort_propagates() {
    let (addr, _svc, shutdown) = spawn_server(Behavior::Abort("nope".to_string())).await;
    let cfg = HooksConfig {
        enabled: true,
        bindings: vec![binding(addr, HookEvent::BeforeChange)],
        ..HooksConfig::default()
    };
    let dispatcher =
        TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).expect("dispatcher");
    let outcome = dispatcher
        .call_before(&cfg.bindings[0], invocation(HookEvent::BeforeChange))
        .await
        .expect("call");
    assert_eq!(outcome.abort_reason.as_deref(), Some("nope"));
    let _ = shutdown.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_change_round_trips() {
    let (addr, svc, shutdown) = spawn_server(Behavior::PassThrough).await;
    let cfg = HooksConfig {
        enabled: true,
        bindings: vec![binding(addr, HookEvent::AfterChange)],
        ..HooksConfig::default()
    };
    let dispatcher =
        TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).expect("dispatcher");
    dispatcher
        .call_after(&cfg.bindings[0], invocation(HookEvent::AfterChange))
        .await
        .expect("call");
    assert_eq!(*svc.after_count.lock().await, 1);
    let _ = shutdown.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_endpoint_yields_unavailable() {
    // Use a port that nothing is listening on (port 1 on loopback).
    let cfg = HooksConfig {
        enabled: true,
        bindings: vec![HookBinding {
            schema: "Translation".to_string(),
            event: HookEvent::BeforeChange,
            endpoint: "http://127.0.0.1:1".to_string(),
            timeout_ms: Some(500),
            required: true,
            descriptor_path: Some(DESCRIPTOR_PATH.to_string()),
        }],
        ..HooksConfig::default()
    };
    let dispatcher =
        TonicHookDispatcher::new(&cfg, TonicDispatcherConfig::default()).expect("dispatcher");
    let err = dispatcher
        .call_before(&cfg.bindings[0], invocation(HookEvent::BeforeChange))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            HookError::Unavailable { .. } | HookError::Timeout { .. }
        ),
        "unexpected error: {err:?}"
    );
}
