use std::fmt;
use std::str::FromStr;

use mti::prelude::{MagicTypeId, MagicTypeIdExt, V7};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A TypeID-based identifier with prefix "entity".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(MagicTypeId);

const PREFIX: &str = "entity";

impl EntityId {
    /// Generates a new random `EntityId` using UUIDv7.
    pub fn new() -> Self {
        Self(PREFIX.create_type_id::<V7>())
    }

    /// Parses an `EntityId` from its string representation, validating the "entity" prefix.
    pub fn parse(s: &str) -> Result<Self, String> {
        let id = MagicTypeId::from_str(s).map_err(|e| format!("{e}"))?;
        if id.prefix().as_str() != PREFIX {
            return Err(format!(
                "expected prefix '{PREFIX}', got '{}'",
                id.prefix().as_str()
            ));
        }
        Ok(Self(id))
    }

    /// Returns the string representation of this id.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for EntityId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for EntityId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_correct_prefix() {
        let id = EntityId::new();
        assert!(
            id.as_str().starts_with("entity_"),
            "expected 'entity_' prefix, got: {}",
            id
        );
    }

    #[test]
    fn parse_valid() {
        let id = EntityId::new();
        let parsed = EntityId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_wrong_prefix() {
        let wrong = "schema_01h455vb4pex5vsknk084sn02q";
        assert!(EntityId::parse(wrong).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let id = EntityId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_matches_as_str() {
        let id = EntityId::new();
        assert_eq!(id.to_string(), id.as_str());
    }
}
