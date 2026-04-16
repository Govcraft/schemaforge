//! Config for named S3-compatible storage backends.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level `[schema_forge.storage]` section.
///
/// Each `[schema_forge.storage.backends.<name>]` table becomes one entry in
/// `backends`. A schema `file(bucket: "<name>", ...)` declaration refers to a
/// named backend here; startup validates that every referenced bucket resolves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Named S3-compatible backends. Keys are referenced from DSL `bucket:`
    /// parameters on `file` fields.
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,

    /// TTL for presigned URLs in seconds when a backend does not override it.
    /// Applied to both presigned PUT (upload) and presigned GET (download) flows.
    #[serde(default = "default_presign_ttl_secs")]
    pub default_presign_ttl_secs: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backends: HashMap::new(),
            default_presign_ttl_secs: default_presign_ttl_secs(),
        }
    }
}

fn default_presign_ttl_secs() -> u64 {
    300
}

/// Configuration for one S3-compatible backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Override endpoint URL. Required for MinIO and Wasabi; omit for AWS S3 so
    /// the SDK picks the regional endpoint automatically.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// AWS region (or any non-empty string for MinIO — MinIO ignores region).
    pub region: String,
    /// Bucket name within the backend.
    pub bucket: String,
    /// Static access key. Omit to use IAM role / env credentials chain.
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// Static secret key paired with `access_key_id`.
    #[serde(default)]
    pub secret_access_key: Option<String>,
    /// Session token for temporary credentials. Omit for long-lived keys.
    #[serde(default)]
    pub session_token: Option<String>,
    /// Force path-style addressing (`http://host/bucket/key`). Required for MinIO.
    /// Defaults to `false` for AWS compatibility.
    #[serde(default)]
    pub force_path_style: bool,
    /// Presign TTL override in seconds. Inherits `default_presign_ttl_secs` when
    /// unset.
    #[serde(default)]
    pub presign_ttl_secs: Option<u64>,
}

impl BackendConfig {
    /// Returns the effective presign TTL, preferring the backend override, then
    /// the caller-supplied fallback.
    pub fn effective_presign_ttl_secs(&self, fallback: u64) -> u64 {
        self.presign_ttl_secs.unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_storage_is_empty_with_300s_ttl() {
        let c = StorageConfig::default();
        assert!(c.backends.is_empty());
        assert_eq!(c.default_presign_ttl_secs, 300);
    }

    #[test]
    fn deserialize_storage_section_with_named_backend() {
        let toml = r#"
            default_presign_ttl_secs = 120

            [backends.documents]
            endpoint = "http://localhost:9000"
            region = "us-gov-west-1"
            bucket = "forge-documents"
            access_key_id = "minio"
            secret_access_key = "minio-secret"
            force_path_style = true
            presign_ttl_secs = 600
        "#;
        let c: StorageConfig = toml::from_str(toml).unwrap();
        assert_eq!(c.default_presign_ttl_secs, 120);
        assert_eq!(c.backends.len(), 1);
        let docs = &c.backends["documents"];
        assert_eq!(docs.endpoint.as_deref(), Some("http://localhost:9000"));
        assert_eq!(docs.bucket, "forge-documents");
        assert_eq!(docs.region, "us-gov-west-1");
        assert!(docs.force_path_style);
        assert_eq!(docs.effective_presign_ttl_secs(300), 600);
    }

    #[test]
    fn backend_inherits_default_ttl_when_not_overridden() {
        let toml = r#"
            [backends.media]
            region = "us-east-1"
            bucket = "forge-media"
        "#;
        let c: StorageConfig = toml::from_str(toml).unwrap();
        let b = &c.backends["media"];
        assert_eq!(b.effective_presign_ttl_secs(42), 42);
        assert!(b.access_key_id.is_none());
        assert!(b.secret_access_key.is_none());
        assert!(!b.force_path_style);
    }

    #[test]
    fn missing_storage_section_returns_default() {
        let c = StorageConfig::default();
        assert_eq!(c.default_presign_ttl_secs, 300);
        assert_eq!(c.backends.len(), 0);
    }
}
