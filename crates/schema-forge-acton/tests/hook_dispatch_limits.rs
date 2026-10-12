//! Background admission follows the deployment's hook configuration.
use acton_service::prelude::{ActonApp, ActorExtension, ActorHandleInterface};
use schema_forge_acton::{
    hooks::{
        DispatchHook, HookBinding, HookDispatcher, HookError, HookInvocation, HookOutcome,
        HooksConfig,
    },
    HookDispatchActor,
};
use schema_forge_core::types::HookEvent;
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, Semaphore};

#[derive(Debug)]
struct BlockingDispatcher {
    started: mpsc::UnboundedSender<()>,
    completed: mpsc::UnboundedSender<()>,
    release: Semaphore,
}
#[async_trait::async_trait]
impl HookDispatcher for BlockingDispatcher {
    async fn call_before(
        &self,
        _: &HookBinding,
        _: HookInvocation,
    ) -> Result<HookOutcome, HookError> {
        Ok(HookOutcome::default())
    }
    async fn call_after(&self, _: &HookBinding, _: HookInvocation) -> Result<(), HookError> {
        self.started.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
        self.completed.send(()).unwrap();
        Ok(())
    }
}

async fn check_limit(configured: usize, expected: usize) {
    let mut runtime = ActonApp::launch_async().await;
    let mut builder = runtime.new_actor::<HookDispatchActor>();
    HookDispatchActor::configure(&mut builder);
    let actor = builder.handle().clone();
    let (started, mut starts) = mpsc::unbounded_channel();
    let (completed, mut finishes) = mpsc::unbounded_channel();
    let dispatcher = Arc::new(BlockingDispatcher {
        started,
        completed,
        release: Semaphore::new(0),
    });
    let config = HooksConfig {
        enabled: true,
        max_concurrent_async: configured,
        bindings: vec![HookBinding {
            schema: "Note".into(),
            event: HookEvent::AfterChange,
            endpoint: "http://unused".into(),
            timeout_ms: None,
            required: false,
            descriptor_path: None,
        }],
        ..Default::default()
    };
    for _ in 0..6 {
        actor
            .send(DispatchHook {
                invocation: HookInvocation {
                    schema: "Note".into(),
                    event: HookEvent::AfterChange,
                    operation: "create".into(),
                    user_id: None,
                    entity_id: None,
                    fields: Default::default(),
                    changed_fields: vec![],
                    previous: Default::default(),
                },
                dispatcher: Some(dispatcher.clone()),
                config: config.clone(),
            })
            .await;
    }
    let actor = builder.start().await;
    for _ in 0..expected {
        tokio::time::timeout(Duration::from_secs(5), starts.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), starts.recv())
            .await
            .is_err()
    );
    dispatcher.release.add_permits(6);
    for _ in 0..6 {
        tokio::time::timeout(Duration::from_secs(5), finishes.recv())
            .await
            .unwrap()
            .unwrap();
    }
    actor.stop().await.unwrap();
    runtime.shutdown_all().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_async_limit_bounds_calls_and_queued_work_finishes() {
    check_limit(2, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_async_limit_still_makes_progress_serially() {
    check_limit(0, 1).await;
}
