//! Optional storage-backed record concurrency contract.

use std::{fmt, str::FromStr};

use schema_forge_core::types::EntityId;

use crate::{BackendError, Entity};

/// An opaque record revision, independent of entity fields and HTTP representations.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EntityRevision(EntityId);

impl EntityRevision {
    /// Generate a fresh revision, including for equal-value accepted writes.
    pub fn fresh() -> Self {
        Self(EntityId::new("revision"))
    }

    /// Return the transport representation.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for EntityRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntityRevision([opaque])")
    }
}

impl fmt::Display for EntityRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EntityRevision {
    type Err = ConditionalMutationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 35 || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(ConditionalMutationError::InvalidCondition);
        }
        let id = EntityId::parse(value).map_err(|_| ConditionalMutationError::InvalidCondition)?;
        if id.prefix() != "revision" || id.as_str() != value {
            return Err(ConditionalMutationError::InvalidCondition);
        }
        Ok(Self(id))
    }
}

/// A record and its revision read from the same storage snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionedEntity {
    /// The ordinary entity, without internal storage metadata.
    pub entity: Entity,
    /// The revision to compare on a later conditional mutation.
    pub revision: EntityRevision,
}

/// Errors for the optional conditional API. Existing backend errors stay unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalMutationError {
    /// This backend or schema has not enabled the contract.
    Unsupported,
    /// The supplied revision does not follow the opaque token format.
    InvalidCondition,
    /// The authorized baseline no longer matches storage.
    Conflict,
    /// The storage operation failed.
    Backend(BackendError),
}

impl fmt::Display for ConditionalMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "conditional entity mutations are not enabled for this schema",
            Self::InvalidCondition => "invalid entity revision",
            Self::Conflict => "the record changed; reload before retrying",
            Self::Backend(_) => "conditional entity storage operation failed",
        })
    }
}

impl std::error::Error for ConditionalMutationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BackendError> for ConditionalMutationError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_round_trip_without_reusing_entity_identity() {
        let first = EntityRevision::fresh();
        let second = EntityRevision::fresh();
        assert_ne!(first, second);
        assert_eq!(first.as_str().parse::<EntityRevision>().unwrap(), first);
        assert_eq!(format!("{first:?}"), "EntityRevision([opaque])");
        for invalid in ["*", "", "revision_x", "W/\"revision_x\""] {
            assert!(invalid.parse::<EntityRevision>().is_err());
        }
        assert!(EntityId::new("contact")
            .as_str()
            .parse::<EntityRevision>()
            .is_err());
    }
}
