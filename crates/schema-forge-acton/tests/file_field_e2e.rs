//! End-to-end test for the file-field storage layer against a real
//! S3-compatible backend (MinIO).
//!
//! The runtime's S3 path is thin on purpose — the interesting behavior is:
//!
//! 1. Config → `StorageRegistry` bootstraps a working client for a named bucket.
//! 2. `presign_put` mints a URL that accepts a subsequent PUT with the declared
//!    `Content-Type`.
//! 3. `head_object` reports the uploaded size + content-type.
//! 4. `presign_get` mints a URL that retrieves the same bytes.
//! 5. `get_object_stream` streams the same bytes back.
//! 6. `copy_object` + `delete_object` work (used by the quarantine path).
//!
//! This test is ignored by default so it never runs in plain `cargo nextest`
//! invocations. To exercise it locally:
//!
//! ```text
//! source scripts/minio-up.sh
//! cargo nextest run --run-ignored all -p schema-forge-acton --test file_field_e2e
//! ```
//!
//! CI environments that cannot start MinIO can skip this test safely; the rest
//! of the storage layer is covered by the unit tests in
//! `src/storage/{config,s3,mod}.rs` and `src/routes/files.rs`.

use std::collections::HashMap;
use std::env;

use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};

use schema_forge_acton::storage::{BackendConfig, S3Client, StorageConfig, StorageRegistry};

const BUCKET_ALIAS: &str = "e2e";

/// Read the six env vars `scripts/minio-up.sh` exports. Returns `None` when
/// any are missing so the test can skip cleanly.
fn e2e_config() -> Option<StorageConfig> {
    let endpoint = env::var("SCHEMAFORGE_E2E_S3_ENDPOINT").ok()?;
    let access = env::var("SCHEMAFORGE_E2E_S3_ACCESS_KEY").ok()?;
    let secret = env::var("SCHEMAFORGE_E2E_S3_SECRET_KEY").ok()?;
    let bucket = env::var("SCHEMAFORGE_E2E_S3_BUCKET").ok()?;
    let region =
        env::var("SCHEMAFORGE_E2E_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());

    let mut backends = HashMap::new();
    backends.insert(
        BUCKET_ALIAS.to_string(),
        BackendConfig {
            endpoint: Some(endpoint),
            region,
            bucket,
            access_key_id: Some(access),
            secret_access_key: Some(secret),
            session_token: None,
            force_path_style: true,
            presign_ttl_secs: Some(300),
        },
    );
    Some(StorageConfig {
        backends,
        default_presign_ttl_secs: 300,
    })
}

fn unique_key(field: &str, filename: &str) -> String {
    format!(
        "tests/{}/{}-{}",
        field,
        uuid::Uuid::now_v7(),
        filename
    )
}

async fn setup_client() -> S3Client {
    let cfg = e2e_config().expect("SCHEMAFORGE_E2E_S3_* env vars not set");
    let registry = StorageRegistry::from_config(&cfg)
        .await
        .expect("storage registry boot");
    registry
        .get(BUCKET_ALIAS)
        .map(|arc| (*arc).clone())
        .expect("bucket alias resolved")
}

#[tokio::test]
#[ignore = "requires MinIO; see scripts/minio-up.sh"]
async fn presigned_put_round_trip() {
    let client = setup_client().await;
    let key = unique_key("round_trip", "hello.txt");
    let body = b"hello, schemaforge file fields";

    // Mint PUT URL and upload via reqwest, mirroring the runtime's client flow.
    let presigned = client
        .presign_put(&key, "text/plain", Some(120))
        .await
        .expect("presign put");
    let http = reqwest::Client::new();
    let put = http
        .put(&presigned.url)
        .header(CONTENT_TYPE, &presigned.content_type)
        .body(body.to_vec())
        .send()
        .await
        .expect("put send");
    assert!(
        put.status().is_success(),
        "PUT returned {}: {}",
        put.status(),
        put.text().await.unwrap_or_default()
    );

    // HEAD verifies size + mime round-trip.
    let head = client
        .head_object(&key)
        .await
        .expect("head_object call")
        .expect("object present");
    assert_eq!(head.size as usize, body.len());
    assert_eq!(head.content_type.as_deref(), Some("text/plain"));
    assert!(head.etag.is_some());

    // Presigned GET returns the same bytes.
    let get_url = client
        .presign_get(&key, Some(60))
        .await
        .expect("presign get");
    let got = http
        .get(&get_url)
        .send()
        .await
        .expect("get send")
        .bytes()
        .await
        .expect("get body");
    assert_eq!(&got[..], &body[..]);

    // Proxied streaming path also returns the same bytes.
    let mut stream = client
        .get_object_stream(&key)
        .await
        .expect("get_object_stream");
    let mut collected: Vec<u8> = Vec::with_capacity(body.len());
    while let Some(chunk) = stream.body.next().await {
        collected.extend_from_slice(&chunk.expect("stream chunk"));
    }
    assert_eq!(collected, body);
    assert_eq!(stream.content_length as usize, body.len());

    client.delete_object(&key).await.expect("cleanup delete");
}

#[tokio::test]
#[ignore = "requires MinIO; see scripts/minio-up.sh"]
async fn head_object_missing_returns_none() {
    let client = setup_client().await;
    let key = format!("tests/missing/{}", uuid::Uuid::now_v7());
    let head = client.head_object(&key).await.expect("head_object call");
    assert!(head.is_none(), "expected None for absent key");
}

#[tokio::test]
#[ignore = "requires MinIO; see scripts/minio-up.sh"]
async fn copy_and_delete_support_quarantine_flow() {
    let client = setup_client().await;
    let src_key = unique_key("quarantine", "payload.bin");
    let dst_key = format!("quarantine/{src_key}");

    // Seed an object via a presigned PUT.
    let body = b"bytes marked quarantined";
    let presigned = client
        .presign_put(&src_key, "application/octet-stream", None)
        .await
        .expect("presign put");
    let http = reqwest::Client::new();
    http.put(&presigned.url)
        .header(CONTENT_TYPE, &presigned.content_type)
        .header(CONTENT_LENGTH, body.len())
        .body(body.to_vec())
        .send()
        .await
        .expect("put send")
        .error_for_status()
        .expect("put status");

    // Copy to the quarantine prefix, then remove the original — matches the
    // path the runtime would take after an `on_scan_complete` quarantine verdict.
    client
        .copy_object(&src_key, &dst_key)
        .await
        .expect("copy_object");
    client
        .delete_object(&src_key)
        .await
        .expect("delete original");

    // Original gone, quarantined copy still readable.
    assert!(client.head_object(&src_key).await.expect("head").is_none());
    let head = client
        .head_object(&dst_key)
        .await
        .expect("head quarantined")
        .expect("quarantined object present");
    assert_eq!(head.size as usize, body.len());

    client.delete_object(&dst_key).await.expect("cleanup");
}

#[tokio::test]
#[ignore = "requires MinIO; see scripts/minio-up.sh"]
async fn storage_registry_reports_all_configured_backends() {
    let cfg = e2e_config().expect("e2e env not set");
    let registry = StorageRegistry::from_config(&cfg).await.unwrap();
    assert!(registry.is_enabled());
    assert_eq!(registry.len(), 1);
    assert!(registry.get(BUCKET_ALIAS).is_some());
    assert!(registry.get("nonexistent").is_none());
}
