//! Optional bounded create reconciliation. Receipts never replace authorization.
use crate::{BackendError, Entity};
use chrono::{DateTime, Utc};
use schema_forge_core::types::{EntityId, SchemaDefinition};
use std::{fmt, str::FromStr};

/// Admission window for a reserved v1 intent.
pub const ADMISSION_SECONDS: i64 = 900;
/// Recovery window, including after the created record is renamed or deleted.
pub const RECOVERY_SECONDS: i64 = 86_400;

/// Server-generated identifier for one create attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateIntentId(EntityId);
impl CreateIntentId {
    /// Generate an intent ID; clients cannot reserve a chosen identifier.
    pub fn fresh() -> Self {
        Self(EntityId::new("createintent"))
    }
    /// Opaque transport representation.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl FromStr for CreateIntentId {
    type Err = CreateIntentError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = EntityId::parse(value).map_err(|_| CreateIntentError::Invalid)?;
        if id.prefix() != "createintent" || id.as_str() != value {
            return Err(CreateIntentError::Invalid);
        }
        Ok(Self(id))
    }
}
/// Authenticated scope and server schema snapshot, never supplied by the client.
#[derive(Debug, Clone)]
pub struct CreateIntentScope {
    /// Stable normalized principal identity.
    pub principal: String,
    /// Selected tenant schema and identifier; empty only for an unscoped schema.
    pub tenant: String,
    /// Registered schema identity and definition at this request.
    pub schema: SchemaDefinition,
}
/// Fingerprint of canonical submitted JSON, excluding server-generated values.
#[derive(Clone, PartialEq, Eq)]
pub struct CreateFingerprint(String);
impl CreateFingerprint {
    /// Validate a v1 SHA-256 hexadecimal fingerprint.
    pub fn parse(value: String) -> Result<Self, CreateIntentError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(CreateIntentError::Invalid);
        }
        Ok(Self(value))
    }
    /// Internal storage representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for CreateFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CreateFingerprint([opaque])")
    }
}
/// Optional backend operations sharing one receipt contract.
#[derive(Debug, Clone)]
pub enum CreateIntentRequest {
    /// Reserve without creating an entity.
    Reserve {
        scope: CreateIntentScope,
        fingerprint: CreateFingerprint,
    },
    /// Read a scoped receipt. This does not authorize reading its entity.
    Read {
        scope: CreateIntentScope,
        id: CreateIntentId,
    },
    /// Atomically insert entity, revision and committed receipt, or reconcile.
    Commit {
        scope: CreateIntentScope,
        id: CreateIntentId,
        fingerprint: CreateFingerprint,
        entity: Entity,
    },
}
/// A retained receipt. Entity data must be fetched with current authorization.
#[derive(Debug, Clone)]
pub struct CreateIntentReceipt {
    /// Reserved intent identity.
    pub id: CreateIntentId,
    /// Deadline for admitting an initial commit.
    pub expires_at: DateTime<Utc>,
    /// Deadline for recovery of this receipt.
    pub recover_until: DateTime<Utc>,
    /// Original created entity, even after deletion; absent while pending.
    pub entity_id: Option<EntityId>,
    /// True only for the transaction that inserted the entity.
    pub created: bool,
    /// Bound input fingerprint for trusted route-level preflight comparisons.
    pub fingerprint: CreateFingerprint,
    /// Whether the uncommitted schema snapshot still matches this request.
    pub definition_matches: bool,
}
/// Fail-closed errors for the optional protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateIntentError {
    /// Adapter does not implement this protocol.
    Unsupported,
    /// Malformed protocol input.
    Invalid,
    /// Unknown, expired or differently scoped intent; never create on absence.
    Unavailable,
    /// Reusing an intent with changed submitted content.
    ContentConflict,
    /// Schema changed since an uncommitted reservation.
    SchemaChanged,
    /// Database failure, including a business uniqueness conflict.
    Backend(BackendError),
}
impl fmt::Display for CreateIntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "create reconciliation unsupported",
            Self::Invalid => "invalid create intent",
            Self::Unavailable => "create intent unavailable",
            Self::ContentConflict => "create intent content changed",
            Self::SchemaChanged => "create intent schema changed",
            Self::Backend(_) => "create intent storage failed",
        })
    }
}
impl std::error::Error for CreateIntentError {}
impl From<BackendError> for CreateIntentError {
    fn from(value: BackendError) -> Self {
        Self::Backend(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_canonical_server_namespaced_values() {
        let id = CreateIntentId::fresh();
        assert_eq!(id.as_str().parse::<CreateIntentId>().unwrap(), id);
        for value in [
            "".into(),
            "garbage".into(),
            format!(" {}", id.as_str()),
            format!("{},{}", id.as_str(), id.as_str()),
            EntityId::new("note").to_string(),
        ] {
            assert!(value.parse::<CreateIntentId>().is_err());
        }
    }
    #[test]
    fn fingerprints_require_exact_lowercase_sha256_shape() {
        assert!(CreateFingerprint::parse("a".repeat(64)).is_ok());
        for value in [
            "a".repeat(63),
            "A".repeat(64),
            "g".repeat(64),
            "a".repeat(65),
        ] {
            assert!(CreateFingerprint::parse(value).is_err());
        }
    }
}
