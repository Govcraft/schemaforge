//! S3-compatible object storage for `file` field types.
//!
//! The runtime never handles bytes on the upload path: clients request a presigned
//! PUT URL, upload directly to the configured bucket, then call the confirm
//! endpoint. Downloads either redirect to a presigned GET URL or stream bytes
//! back through the runtime, depending on the per-field `access` DSL setting.
//!
//! All backends are S3-compatible (AWS S3, MinIO, Cloudflare R2, Wasabi, etc.).
//! Named backends are declared in config; each `file` field references one by
//! name, which lets ops swap endpoints between environments without schema edits.

pub mod config;
pub mod s3;

use std::collections::HashMap;
use std::sync::Arc;

pub use config::{BackendConfig, StorageConfig};
pub use s3::{PresignedUpload, S3Client, StorageError};

use async_trait::async_trait;

/// Storage sink for generated export artifacts.
///
/// The async export-job path (ADR-0003) generates a CSV/NDJSON file inside the
/// runtime and lands it in object storage, then hands the caller a TTL-bounded
/// presigned GET URL. This trait is the seam the job actor depends on so the
/// upload + presign pair can be mocked in tests — the integration suite uses an
/// in-memory implementation rather than a live MinIO/S3 endpoint.
#[async_trait]
pub trait ExportArtifactStore: Send + Sync + std::fmt::Debug {
    /// Write `bytes` under `key` with the given `content_type`. Overwrites any
    /// object already at that key (export keys embed a fresh job id, so this is
    /// effectively write-once).
    async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str)
        -> Result<(), StorageError>;

    /// Mint a presigned GET URL for `key`, valid for at most `ttl_secs` seconds
    /// (the backend clamps to its own ceiling). Pass `None` for the backend
    /// default TTL.
    async fn presign_get(&self, key: &str, ttl_secs: Option<u64>)
        -> Result<String, StorageError>;
}

#[async_trait]
impl ExportArtifactStore for S3Client {
    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), StorageError> {
        S3Client::put_object(self, key, bytes, content_type).await
    }

    async fn presign_get(
        &self,
        key: &str,
        ttl_secs: Option<u64>,
    ) -> Result<String, StorageError> {
        S3Client::presign_get(self, key, ttl_secs).await
    }
}

/// Registry mapping bucket names to their initialized S3 clients.
///
/// Built at extension setup from [`StorageConfig`]. Cloning shares the underlying
/// map cheaply (individual clients are already `Arc`-wrapped).
#[derive(Debug, Clone, Default)]
pub struct StorageRegistry {
    backends: Arc<HashMap<String, Arc<S3Client>>>,
}

impl StorageRegistry {
    /// Build a registry from a validated `StorageConfig`, initializing one client
    /// per declared backend. Performs no network I/O; credentials and endpoints
    /// are recorded for later presigning and streaming.
    pub async fn from_config(config: &StorageConfig) -> Result<Self, StorageError> {
        let mut backends = HashMap::with_capacity(config.backends.len());
        for (name, cfg) in &config.backends {
            let client = S3Client::from_backend_config(name, cfg, config.default_presign_ttl_secs)
                .await?;
            backends.insert(name.clone(), Arc::new(client));
        }
        Ok(Self {
            backends: Arc::new(backends),
        })
    }

    /// Returns the client bound to `name`, or `None` if no backend with that name
    /// exists in the registry.
    pub fn get(&self, name: &str) -> Option<Arc<S3Client>> {
        self.backends.get(name).cloned()
    }

    /// Returns the sole registered backend when exactly one is configured.
    ///
    /// Used by the async export path to pick a default artifact bucket when the
    /// deployment has not named a dedicated `"exports"` backend: a single-backend
    /// deployment is unambiguous, while a multi-backend one must name `"exports"`
    /// explicitly rather than have a bucket guessed for it.
    pub fn single_backend(&self) -> Option<Arc<S3Client>> {
        if self.backends.len() == 1 {
            self.backends.values().next().cloned()
        } else {
            None
        }
    }

    /// Returns `true` when at least one backend is registered.
    pub fn is_enabled(&self) -> bool {
        !self.backends.is_empty()
    }

    /// Number of registered backends.
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// Returns `true` when no backends are registered.
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_is_not_enabled() {
        let reg = StorageRegistry::default();
        assert!(!reg.is_enabled());
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("documents").is_none());
    }
}
