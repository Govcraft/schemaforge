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
