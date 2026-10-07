//! Persistent SurrealDB revocation using acton-service's storage format.
//!
//! The 0.47.0 writer evaluates its duplicate expression on the first insert,
//! when the existing stamp is absent. Use the supplied stamp as that fallback.

use acton_service::{
    error::Error,
    middleware::{
        revocation::{RevocationNamespace, SurrealTokenRevocation},
        TokenRevocation,
    },
    surrealdb_backend::SurrealClient,
};
use std::sync::Arc;

/// Durable token and subject revocation on the entity database connection.
pub struct SurrealRevocation {
    client: Arc<SurrealClient>,
    namespace: RevocationNamespace,
    native: SurrealTokenRevocation,
}

impl SurrealRevocation {
    /// Use an existing connection and a validated deployment namespace.
    pub fn new(client: Arc<SurrealClient>, namespace: RevocationNamespace) -> Self {
        Self {
            native: SurrealTokenRevocation::new(client.clone(), namespace.clone()),
            client,
            namespace,
        }
    }

    async fn read(&self, kind: &str, identifier: &str) -> Result<Option<i64>, Error> {
        let mut response = self
            .client
            .query(
                "LET $schema = (INFO FOR TABLE token_revocations); \
             IF $schema.fields.stamp IS NONE { THROW 'token revocation schema is unavailable'; }; \
             SELECT VALUE stamp FROM token_revocations \
             WHERE namespace=$namespace AND kind=$kind AND identifier=$identifier;",
            )
            .bind(("namespace", self.namespace.as_str().to_owned()))
            .bind(("kind", kind.to_owned()))
            .bind(("identifier", identifier.to_owned()))
            .await
            .map_err(storage_error)?
            .check()
            .map_err(storage_error)?;
        let rows: Vec<i64> = response.take(2).map_err(storage_error)?;
        Ok(rows.first().copied())
    }

    async fn write(&self, kind: &str, identifier: &str, stamp: i64) -> Result<(), Error> {
        self.client
            .query(
                "LET $schema = (INFO FOR TABLE token_revocations); \
             IF $schema.fields.stamp IS NONE { THROW 'token revocation schema is unavailable'; }; \
             INSERT INTO token_revocations (namespace,kind,identifier,stamp) \
             VALUES ($namespace,$kind,$identifier,$stamp) \
             ON DUPLICATE KEY UPDATE stamp = math::max([stamp ?? $stamp, $stamp]);",
            )
            .bind(("namespace", self.namespace.as_str().to_owned()))
            .bind(("kind", kind.to_owned()))
            .bind(("identifier", identifier.to_owned()))
            .bind(("stamp", stamp))
            .await
            .map_err(storage_error)?
            .check()
            .map_err(storage_error)?;
        Ok(())
    }
}

fn storage_error(error: impl std::fmt::Display) -> Error {
    Error::Internal(format!("SurrealDB revocation storage: {error}"))
}

#[async_trait::async_trait]
impl TokenRevocation for SurrealRevocation {
    async fn initialize(&self) -> Result<(), Error> {
        self.native.initialize().await
    }

    async fn cleanup_expired(&self) -> Result<(), Error> {
        self.native.cleanup_expired().await
    }

    async fn is_revoked(&self, jti: &str) -> Result<bool, Error> {
        Ok(self
            .read("token", jti)
            .await?
            .is_some_and(|stamp| stamp > chrono::Utc::now().timestamp()))
    }

    async fn revoke(&self, jti: &str, ttl_secs: u64) -> Result<(), Error> {
        let ttl = i64::try_from(ttl_secs).map_err(storage_error)?;
        let stamp = chrono::Utc::now()
            .timestamp()
            .checked_add(ttl)
            .ok_or_else(|| {
                Error::Internal("revocation expiration exceeds timestamp range".into())
            })?;
        self.write("token", jti, stamp).await
    }

    async fn subject_not_before(&self, subject: &str) -> Result<Option<i64>, Error> {
        self.read("subject", subject).await
    }

    async fn revoke_subject(&self, subject: &str, not_before: i64) -> Result<(), Error> {
        self.write("subject", subject, not_before).await
    }
}
