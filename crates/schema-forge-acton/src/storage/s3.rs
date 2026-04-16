//! Thin wrapper over `aws-sdk-s3` covering the operations SchemaForge needs:
//! presigned PUT URLs for uploads, presigned GET URLs for presigned-mode
//! downloads, `HeadObject` for confirm-time size verification, and streaming
//! `GetObject` for proxied-mode downloads.
//!
//! The wrapper exists so routes do not couple directly to the SDK's surface and
//! so test-time construction is straightforward.

use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;

use super::config::BackendConfig;

/// Maximum TTL a presigned URL may have. AWS enforces 7 days; we clamp defensively.
const MAX_PRESIGN_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Client bound to a single backend (bucket + credentials).
///
/// Cheaply cloneable: the SDK `Client` already shares connection pooling via its
/// inner `Arc`. Each named backend in [`super::StorageRegistry`] holds one of
/// these wrapped in `Arc` for cross-handler sharing.
#[derive(Debug, Clone)]
pub struct S3Client {
    name: String,
    bucket: String,
    default_ttl_secs: u64,
    inner: Client,
}

impl S3Client {
    /// Initialize a client for the given backend config.
    ///
    /// Static credentials are used when both `access_key_id` and
    /// `secret_access_key` are set; otherwise the default AWS credentials
    /// chain runs (env vars, IAM role, SSO, etc.).
    pub async fn from_backend_config(
        name: &str,
        cfg: &BackendConfig,
        fallback_ttl_secs: u64,
    ) -> Result<Self, StorageError> {
        let region = Region::new(cfg.region.clone());
        let mut loader =
            aws_config::defaults(BehaviorVersion::latest()).region(region.clone());

        if let (Some(access), Some(secret)) = (&cfg.access_key_id, &cfg.secret_access_key) {
            loader = loader.credentials_provider(Credentials::new(
                access.clone(),
                secret.clone(),
                cfg.session_token.clone(),
                None,
                "schema-forge-static",
            ));
        }

        let shared = loader.load().await;
        let mut s3_builder = aws_sdk_s3::config::Builder::from(&shared).region(region);
        if let Some(endpoint) = &cfg.endpoint {
            s3_builder = s3_builder.endpoint_url(endpoint);
        }
        if cfg.force_path_style {
            s3_builder = s3_builder.force_path_style(true);
        }

        Ok(Self {
            name: name.to_string(),
            bucket: cfg.bucket.clone(),
            default_ttl_secs: cfg.effective_presign_ttl_secs(fallback_ttl_secs),
            inner: Client::from_conf(s3_builder.build()),
        })
    }

    /// Human-readable backend name (e.g., `"documents"`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Bucket this client writes into.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Default presign TTL for this backend (after honoring backend overrides).
    pub fn default_ttl_secs(&self) -> u64 {
        self.default_ttl_secs
    }

    /// Mint a presigned PUT URL scoped to `key` with an exact
    /// `Content-Type` condition. The client MUST send the returned
    /// `content_type` as its `Content-Type` request header on the subsequent
    /// PUT or the signature will not match.
    ///
    /// `ttl_secs` may override the backend default; supply `None` to use it
    /// unchanged. Exceeding [`MAX_PRESIGN_TTL_SECS`] is clamped.
    pub async fn presign_put(
        &self,
        key: &str,
        content_type: &str,
        ttl_secs: Option<u64>,
    ) -> Result<PresignedUpload, StorageError> {
        let ttl = clamp_ttl(ttl_secs.unwrap_or(self.default_ttl_secs));
        let config = PresigningConfig::expires_in(Duration::from_secs(ttl))
            .map_err(|e| StorageError::Presign(e.to_string()))?;

        let presigned = self
            .inner
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .presigned(config)
            .await
            .map_err(map_sdk_error)?;

        Ok(PresignedUpload {
            url: presigned.uri().to_string(),
            key: key.to_string(),
            content_type: content_type.to_string(),
            expires_in_secs: ttl,
        })
    }

    /// Mint a presigned GET URL scoped to `key`. `ttl_secs` has the same
    /// semantics as [`Self::presign_put`].
    pub async fn presign_get(
        &self,
        key: &str,
        ttl_secs: Option<u64>,
    ) -> Result<String, StorageError> {
        let ttl = clamp_ttl(ttl_secs.unwrap_or(self.default_ttl_secs));
        let config = PresigningConfig::expires_in(Duration::from_secs(ttl))
            .map_err(|e| StorageError::Presign(e.to_string()))?;

        let presigned = self
            .inner
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(config)
            .await
            .map_err(map_sdk_error)?;

        Ok(presigned.uri().to_string())
    }

    /// `HeadObject` to verify the object exists and read its size + content-type
    /// at confirm time. Returns `None` when the object is absent so the caller
    /// can distinguish "not yet uploaded" from a genuine error.
    pub async fn head_object(&self, key: &str) -> Result<Option<HeadInfo>, StorageError> {
        let resp = self
            .inner
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match resp {
            Ok(out) => Ok(Some(HeadInfo {
                size: u64::try_from(out.content_length().unwrap_or_default()).unwrap_or(0),
                content_type: out.content_type().map(str::to_string),
                etag: out.e_tag().map(str::to_string),
            })),
            Err(err) => match err {
                SdkError::ServiceError(svc) if matches!(svc.err(), HeadObjectError::NotFound(_)) => {
                    Ok(None)
                }
                other => Err(map_sdk_error(other)),
            },
        }
    }

    /// Open a streaming `GetObject` for proxied downloads. The returned
    /// [`ByteStream`] is polled by the Axum response body.
    pub async fn get_object_stream(&self, key: &str) -> Result<ObjectStream, StorageError> {
        let resp = self
            .inner
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(map_sdk_error)?;

        Ok(ObjectStream {
            content_type: resp.content_type().map(str::to_string),
            content_length: u64::try_from(resp.content_length().unwrap_or_default())
                .unwrap_or(0),
            body: resp.body,
        })
    }

    /// Copy an object to a new key within the same bucket. Used when the scan
    /// path moves a quarantined object under a `quarantine/` prefix.
    pub async fn copy_object(&self, src_key: &str, dst_key: &str) -> Result<(), StorageError> {
        self.inner
            .copy_object()
            .copy_source(format!("{}/{}", self.bucket, src_key))
            .bucket(&self.bucket)
            .key(dst_key)
            .send()
            .await
            .map_err(map_sdk_error)?;
        Ok(())
    }

    /// Permanently delete an object by key. Used to remove the original after
    /// a successful quarantine copy, or on administrative cleanup.
    pub async fn delete_object(&self, key: &str) -> Result<(), StorageError> {
        self.inner
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(map_sdk_error)?;
        Ok(())
    }
}

/// Successful presigned-PUT mint payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUpload {
    /// Fully-qualified URL the client PUTs bytes to.
    pub url: String,
    /// Object key the caller should persist alongside attachment metadata.
    pub key: String,
    /// Exact `Content-Type` header the client must send.
    pub content_type: String,
    /// Expiry of the URL in seconds from now.
    pub expires_in_secs: u64,
}

/// Output of [`S3Client::head_object`] when the object exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    pub size: u64,
    pub content_type: Option<String>,
    pub etag: Option<String>,
}

/// Output of [`S3Client::get_object_stream`]: a streaming body plus the metadata
/// that callers need to forward as response headers.
#[derive(Debug)]
pub struct ObjectStream {
    pub content_type: Option<String>,
    pub content_length: u64,
    pub body: ByteStream,
}

/// Errors surfaced by [`S3Client`].
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Failed to mint a presigned URL (expiry too long, missing signer, etc.).
    #[error("presign error: {0}")]
    Presign(String),
    /// Bucket rejected the request (permission, missing bucket, etc.).
    #[error("s3 request failed: {0}")]
    Request(String),
    /// Something unexpected bubbled from the SDK.
    #[error("s3 sdk error: {0}")]
    Sdk(String),
}

fn clamp_ttl(ttl: u64) -> u64 {
    ttl.clamp(1, MAX_PRESIGN_TTL_SECS)
}

fn map_sdk_error<E, R>(err: SdkError<E, R>) -> StorageError
where
    E: std::fmt::Debug,
    R: std::fmt::Debug,
{
    match err {
        SdkError::ServiceError(svc) => StorageError::Request(format!("{:?}", svc.err())),
        SdkError::TimeoutError(_) => StorageError::Sdk("request timed out".into()),
        SdkError::DispatchFailure(d) => StorageError::Sdk(format!("dispatch failure: {d:?}")),
        SdkError::ResponseError(r) => StorageError::Sdk(format!("response error: {r:?}")),
        SdkError::ConstructionFailure(c) => {
            StorageError::Sdk(format!("construction failure: {c:?}"))
        }
        other => StorageError::Sdk(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_ttl_enforces_bounds() {
        assert_eq!(clamp_ttl(0), 1);
        assert_eq!(clamp_ttl(120), 120);
        assert_eq!(clamp_ttl(MAX_PRESIGN_TTL_SECS + 10), MAX_PRESIGN_TTL_SECS);
    }
}
