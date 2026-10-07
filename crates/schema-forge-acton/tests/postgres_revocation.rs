//! Revocation API and shared HTTP authentication on a real PostgreSQL connection.
#![cfg(feature = "postgres")]

use acton_service::{
    auth::{
        config::TokenGenerationConfig,
        tokens::{paseto_generator::PasetoGenerator, ClaimsBuilder, TokenGenerator},
    },
    config::{Config, PasetoConfig, TokenConfig},
    middleware::{
        revocation::{PgTokenRevocation, RevocationNamespace},
        PasetoAuth, TokenRevocation,
    },
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{routes::revocations::revoke_subject, SchemaForgeConfig};
use std::sync::Arc;
use tower::ServiceExt;

#[path = "support/postgres.rs"]
mod postgres;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated SCHEMAFORGE_TEST_POSTGRES_URL; creates token_revocations"]
async fn postgres_revocation_persists_and_enforces_monotonic_cutoffs() {
    let backend = schema_forge_postgres::PgBackend::connect(&postgres::isolated_url(
        "SCHEMAFORGE_TEST_POSTGRES_REVOCATION_URL",
    ))
    .await
    .unwrap();
    let namespace = RevocationNamespace::new(format!("test_{}", uuid::Uuid::new_v4())).unwrap();
    let provider = Arc::new(PgTokenRevocation::new(
        backend.pool().clone(),
        namespace.clone(),
    ));
    provider.initialize().await.unwrap();
    let key = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(key.path(), [29; 32]).unwrap();
    let token_config = PasetoConfig {
        key_path: key.path().to_owned(),
        ..PasetoConfig::default()
    };
    let config = Config::<SchemaForgeConfig> {
        token: Some(TokenConfig::Paseto(token_config.clone())),
        ..Config::default()
    };
    let service = ServiceBuilder::new()
        .with_config(config)
        .with_token_revocation(provider.clone())
        .build();
    let validator = PasetoAuth::new(&token_config)
        .unwrap()
        .with_shared_revocation(provider.clone());
    let app = Router::new()
        .route("/protected", get(|| async { StatusCode::OK }))
        .route("/auth/revocations", post(revoke_subject))
        .layer(axum::middleware::from_fn_with_state(
            validator,
            PasetoAuth::middleware,
        ))
        .with_state(service.state().clone());
    let generator = PasetoGenerator::with_symmetric_key([29; 32], TokenGenerationConfig::default());
    let admin = generator
        .generate_token(
            &ClaimsBuilder::new()
                .user("admin")
                .role("platform_admin")
                .build()
                .unwrap(),
        )
        .unwrap();
    let user_claims = ClaimsBuilder::new().user("revoked").build().unwrap();
    let user = generator.generate_token(&user_claims).unwrap();
    let protected = |token: &str| {
        Request::builder()
            .uri("/protected")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(protected(&user))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let cutoff = chrono::Utc::now().timestamp();
    for stamp in [cutoff, cutoff - 100] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/revocations")
                    .header("authorization", format!("Bearer {admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"subject":"user:revoked", "not_before":stamp})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(result["not_before"], cutoff);
    }
    assert_eq!(
        app.clone()
            .oneshot(protected(&user))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let reconnected = PgTokenRevocation::new(backend.pool().clone(), namespace.clone());
    assert_eq!(
        reconnected
            .subject_not_before("user:revoked")
            .await
            .unwrap(),
        Some(cutoff)
    );
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let newer = generator.generate_token(&user_claims).unwrap();
    assert_eq!(
        app.clone()
            .oneshot(protected(&newer))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let another = PgTokenRevocation::new(
        backend.pool().clone(),
        RevocationNamespace::new("unrelated_service").unwrap(),
    );
    assert_eq!(
        another.subject_not_before("user:revoked").await.unwrap(),
        None
    );
}
