use std::fmt;
use std::str::FromStr;

use mti::prelude::{MagicTypeId, MagicTypeIdExt, V7};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A TypeID-based identifier with prefix "schema".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaId(MagicTypeId);

const PREFIX: &str = "schema";

impl SchemaId {
    /// Generates a new random `SchemaId` using UUIDv7.
    pub fn new() -> Self {
        Self(PREFIX.create_type_id::<V7>())
    }

    /// Parses a `SchemaId` from its string representation, validating the "schema" prefix.
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

impl Default for SchemaId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SchemaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for SchemaId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for SchemaId {
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
        let id = SchemaId::new();
        assert!(
            id.as_str().starts_with("schema_"),
            "expected 'schema_' prefix, got: {}",
            id
        );
    }

    #[test]
    fn parse_valid() {
        let id = SchemaId::new();
        let parsed = SchemaId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_wrong_prefix() {
        let wrong = "entity_01h455vb4pex5vsknk084sn02q";
        assert!(SchemaId::parse(wrong).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let id = SchemaId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: SchemaId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_matches_as_str() {
        let id = SchemaId::new();
        assert_eq!(id.to_string(), id.as_str());
    }
}
