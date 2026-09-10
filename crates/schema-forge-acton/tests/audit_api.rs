//! Synthetic disposable storage exercises the mounted HTTP contract and upstream verifier.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use acton_service::audit::{
    AuditChain, AuditConfig, AuditEvent, AuditEventKind, AuditSeverity, AuditStorage,
};
use acton_service::config::Config;
use acton_service::error::Error;
use acton_service::middleware::Claims;
use acton_service::state::AppState;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_acton::routes::forge_routes;
use serde_json::{json, Value};
use tower::ServiceExt;

#[derive(Default)]
struct Store {
    events: Mutex<Vec<AuditEvent>>,
    unavailable: bool,
    query_stalls: bool,
}

#[async_trait]
impl AuditStorage for Store {
    async fn append(&self, event: &AuditEvent) -> Result<(), Error> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
    async fn latest(&self) -> Result<Option<AuditEvent>, Error> {
        Ok(self.events.lock().unwrap().last().cloned())
    }
    async fn query_range(
        &self,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
        _: usize,
    ) -> Result<Vec<AuditEvent>, Error> {
        panic!("HTTP must not use timestamp pagination")
    }
    async fn verify_chain(&self, _: u64) -> Result<Option<u64>, Error> {
        panic!("HTTP must not invoke unbounded verification")
    }
    async fn query_sequence(
        &self,
        from: u64,
        to: u64,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, Error> {
        if self.unavailable {
            return Err(Error::Internal("SECRET storage connection details".into()));
        }
        if self.query_stalls {
            std::future::pending::<()>().await;
        }
        assert!(limit <= 1001, "HTTP exceeded its read bound");
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.sequence >= from && event.sequence <= to)
            .take(limit)
            .cloned()
            .collect())
    }
    async fn sequence_bounds(&self) -> Result<Option<(u64, u64)>, Error> {
        if self.unavailable {
            return Err(Error::Internal("SECRET storage connection details".into()));
        }
        let events = self.events.lock().unwrap();
        Ok(events
            .first()
            .zip(events.last())
            .map(|(first, last)| (first.sequence, last.sequence)))
    }
}

fn fixture(count: usize) -> (Arc<Store>, AuditChain) {
    let mut chain = AuditChain::new("synthetic".into());
    let events = (0..count)
        .map(|_| {
            let mut event = AuditEvent::new(
                AuditEventKind::HttpRequest,
                AuditSeverity::Informational,
                "synthetic".into(),
            );
            event.metadata = Some(json!({"password": "SECRET"}));
            event.path = Some("/SECRET?token=SECRET".into());
            event.source.subject = Some("SECRET subject".into());
            chain.seal(event)
        })
        .collect();
    (
        Arc::new(Store {
            events: Mutex::new(events),
            unavailable: false,
            query_stalls: false,
        }),
        chain,
    )
}

fn claims(role: &str, tenant: &str) -> Claims {
    Claims {
        sub: "synthetic-admin".into(),
        roles: vec![role.into()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::from([("tenant_id".into(), json!(tenant))]),
    }
}

async fn app(store: Option<Arc<Store>>, caller: Option<Claims>, enabled: bool) -> Router {
    let config = Config::<SchemaForgeConfig> {
        audit: Some(AuditConfig {
            enabled,
            ..AuditConfig::default()
        }),
        ..Config::default()
    };
    let mut builder = AppState::builder().config(config).without_tracing();
    if let Some(store) = store {
        builder = builder.audit_storage(store);
    }
    let state = builder.build().await.unwrap();
    let router = Router::new()
        .nest("/api/v1/forge", forge_routes())
        .with_state(state);
    if let Some(caller) = caller {
        router.layer(Extension(caller))
    } else {
        router
    }
}

async fn request(app: &Router, endpoint: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(format!("/api/v1/forge/audit/{endpoint}"));
    let body = if let Some(body) = body {
        request = request
            .method("POST")
            .header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn verification(app: &Router, from: u64, to: u64) -> (StatusCode, Value) {
    request(
        app,
        "verify",
        Some(json!({"from_sequence": from, "to_sequence": to})),
    )
    .await
}

#[tokio::test]
async fn anonymous_and_tenant_roles_cannot_access_any_audit_surface() {
    let (store, _) = fixture(3);
    for (caller, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(claims("tenant_admin", "a")), StatusCode::FORBIDDEN),
        (Some(claims("admin", "b")), StatusCode::FORBIDDEN),
    ] {
        let app = app(Some(store.clone()), caller, true).await;
        for endpoint in ["status", "events", "events?limit=not-a-number"] {
            assert_eq!(request(&app, endpoint, None).await.0, expected);
        }
        assert_eq!(verification(&app, 1, 3).await.0, expected);
        assert_eq!(
            request(&app, "verify", Some(json!({"invalid": true})))
                .await
                .0,
            expected
        );
    }
}

#[tokio::test]
async fn platform_admin_access_is_explicitly_deployment_wide() {
    let (store, _) = fixture(2);
    for tenant in ["a", "b"] {
        let app = app(
            Some(store.clone()),
            Some(claims("platform_admin", tenant)),
            true,
        )
        .await;
        let (code, status) = request(&app, "status", None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(status["scope"], "deployment");
        assert_eq!(status["availability"], "available");
        assert_eq!(
            status["retained_range"],
            json!({"from_sequence":1,"to_sequence":2})
        );
        assert_eq!(
            status["collection"]["mutation_success_acknowledges_persistence"],
            false
        );
        assert_eq!(
            request(&app, "events", None).await.1["events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn status_distinguishes_disabled_missing_failed_and_empty_storage() {
    for (store, enabled, expected) in [
        (Some(fixture(1).0), false, "disabled"),
        (None, true, "unavailable"),
        (
            Some(Arc::new(Store {
                unavailable: true,
                ..Store::default()
            })),
            true,
            "unavailable",
        ),
        (Some(fixture(0).0), true, "empty"),
    ] {
        let app = app(store, Some(claims("platform_admin", "a")), enabled).await;
        let (_, status) = request(&app, "status", None).await;
        assert_eq!(status["availability"], expected);
        assert_eq!(status["retained_range"], Value::Null);
        let (code, result) = verification(&app, 1, 1).await;
        if expected == "empty" {
            assert_eq!(result["outcome"], "incomplete");
        } else {
            assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(result["outcome"], "unavailable");
        }
        assert!(!result.to_string().contains("SECRET"));
    }
}

#[tokio::test]
async fn pagination_excludes_concurrent_appends_and_never_exposes_private_fields() {
    let (store, mut chain) = fixture(5);
    let app = app(
        Some(store.clone()),
        Some(claims("platform_admin", "a")),
        true,
    )
    .await;
    let (code, first) = request(&app, "events?limit=2", None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(first["through_sequence"], 5);
    assert_eq!(first["next_after_sequence"], 2);
    let event = chain.seal(AuditEvent::new(
        AuditEventKind::AuthLoginSuccess,
        AuditSeverity::Informational,
        "synthetic".into(),
    ));
    store.append(&event).await.unwrap();
    let mut all = first["events"].as_array().unwrap().clone();
    let mut after = 2;
    loop {
        let (_, page) = request(
            &app,
            &format!("events?after_sequence={after}&through_sequence=5&limit=2"),
            None,
        )
        .await;
        all.extend(page["events"].as_array().unwrap().clone());
        if let Some(next) = page["next_after_sequence"].as_u64() {
            after = next;
        } else {
            break;
        }
    }
    assert_eq!(
        all.iter()
            .map(|event| event["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert!(!serde_json::to_string(&all).unwrap().contains("SECRET"));
    for event in all {
        assert_eq!(event.as_object().unwrap().len(), 8);
        for excluded in ["metadata", "source", "path", "method", "duration_ms"] {
            assert!(event.get(excluded).is_none());
        }
    }
}

#[tokio::test]
async fn retention_and_gaps_refuse_silent_pagination_omissions() {
    let (store, _) = fixture(5);
    let app = app(
        Some(store.clone()),
        Some(claims("platform_admin", "a")),
        true,
    )
    .await;
    store.events.lock().unwrap().drain(..3);
    assert_eq!(
        request(&app, "events?through_sequence=2", None).await.0,
        StatusCode::CONFLICT
    );
    let (code, body) = request(&app, "events?after_sequence=2&through_sequence=5", None).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["reason"], "audit_snapshot_incomplete");
    let (_, first) = request(&app, "events", None).await;
    assert_eq!(first["events"][0]["sequence"], 4);
    assert_eq!(verification(&app, 4, 5).await.1["outcome"], "incomplete");
    assert_eq!(verification(&app, 5, 5).await.1["outcome"], "valid");
    store.events.lock().unwrap().clear();
    assert_eq!(
        request(&app, "events?after_sequence=4&through_sequence=5", None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (store, _) = fixture(5);
    store.events.lock().unwrap().remove(2);
    let app = self::app(Some(store), Some(claims("platform_admin", "a")), true).await;
    assert_eq!(request(&app, "events", None).await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn valid_full_chain_and_suffixes_report_requested_scope_and_anchor() {
    let (store, _) = fixture(5);
    let app = app(Some(store), Some(claims("platform_admin", "a")), true).await;
    for from in 1..=5 {
        let (code, result) = verification(&app, from, 5).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(result["outcome"], "valid");
        assert_eq!(
            result["checked_range"],
            json!({"from_sequence":from,"to_sequence":5})
        );
        assert_eq!(result["anchor"]["sequence"], from - 1);
        assert_eq!(result["anchor"]["independently_trusted"], false);
    }
    assert_eq!(verification(&app, 6, 6).await.1["outcome"], "incomplete");
    assert_eq!(verification(&app, 1, 6).await.1["outcome"], "incomplete");
}

#[tokio::test]
async fn altered_payload_links_and_anchor_are_broken_but_outside_range_is_not_claimed() {
    for index in 0..5 {
        let (store, _) = fixture(5);
        store.events.lock().unwrap()[index].metadata = Some(json!({"tampered": true}));
        let app = app(Some(store), Some(claims("platform_admin", "a")), true).await;
        let (_, result) = verification(&app, 2, 5).await;
        assert_eq!(result["outcome"], "broken");
        assert_eq!(result["broken_sequence"], index as u64 + 1);
        if index == 0 {
            assert_eq!(result["checked_range"], Value::Null);
        }
    }
    let (store, _) = fixture(5);
    store.events.lock().unwrap()[4].previous_hash = Some("tampered".into());
    let app = app(Some(store), Some(claims("platform_admin", "a")), true).await;
    assert_eq!(verification(&app, 1, 4).await.1["outcome"], "valid");
    assert_eq!(verification(&app, 1, 5).await.1["outcome"], "broken");
}

#[tokio::test]
async fn limits_unknown_filters_and_overflow_are_rejected() {
    let app = app(
        Some(fixture(3).0),
        Some(claims("platform_admin", "a")),
        true,
    )
    .await;
    for query in [
        "limit=0",
        "limit=201",
        "limit=-1",
        "tenant=a",
        "kind=http.request",
        "after_sequence=1",
        "after_sequence=3&through_sequence=2",
        "after_sequence=18446744073709551615&through_sequence=0",
    ] {
        assert_eq!(
            request(&app, &format!("events?{query}"), None).await.0,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    for (from, to) in [(0, 1), (2, 1), (1, 1001), (1, u64::MAX)] {
        assert_eq!(
            verification(&app, from, to).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &app,
            "verify",
            Some(json!({"from_sequence":1,"to_sequence":2,"tenant":"a"}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn absent_audit_configuration_uses_framework_enabled_default() {
    let config = Config::<SchemaForgeConfig> {
        audit: None,
        ..Config::default()
    };
    let state = AppState::builder()
        .config(config)
        .audit_storage(fixture(2).0)
        .without_tracing()
        .build()
        .await
        .unwrap();
    let app = Router::new()
        .nest("/api/v1/forge", forge_routes())
        .with_state(state)
        .layer(Extension(claims("platform_admin", "a")));
    let (_, status) = request(&app, "status", None).await;
    assert_eq!(status["availability"], "available");
    assert_eq!(status["collection"]["configured"], true);
    assert_eq!(verification(&app, 1, 2).await.1["outcome"], "valid");
}

#[tokio::test]
async fn stalled_queries_end_at_deadline_without_claiming_validity() {
    let (fixture, _) = fixture(2);
    let store = Arc::new(Store {
        events: Mutex::new(fixture.events.lock().unwrap().clone()),
        query_stalls: true,
        ..Store::default()
    });
    let app = app(Some(store), Some(claims("platform_admin", "a")), true).await;
    let (events, verification) =
        tokio::join!(request(&app, "events", None), verification(&app, 1, 2));
    assert_eq!(events.0, StatusCode::BAD_GATEWAY);
    assert_eq!(verification.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(verification.1["outcome"], "unavailable");
    assert_eq!(verification.1["checked_range"], Value::Null);
}

#[tokio::test]
async fn maximum_verification_range_and_empty_snapshot_have_defined_results() {
    let app = app(
        Some(fixture(1001).0),
        Some(claims("platform_admin", "a")),
        true,
    )
    .await;
    assert_eq!(verification(&app, 2, 1001).await.1["outcome"], "valid");
    let app = self::app(
        Some(fixture(0).0),
        Some(claims("platform_admin", "a")),
        true,
    )
    .await;
    let (code, page) = request(&app, "events", None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(page["through_sequence"], 0);
    assert_eq!(page["events"], json!([]));
    assert_eq!(page["next_after_sequence"], Value::Null);
    assert_eq!(page["retained_range"], Value::Null);
}
