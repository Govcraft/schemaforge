use std::fmt;
use std::str::FromStr;

use mti::prelude::{MagicTypeId, MagicTypeIdExt, V7};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A TypeID-based identifier whose prefix encodes the entity type
/// (e.g. `project_01k…`, `opportunity_01k…`, `user_01k…`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(MagicTypeId);

impl EntityId {
    /// Generates a new random `EntityId` using UUIDv7 and the given prefix.
    ///
    /// The prefix is sanitized by mti into a valid TypeID prefix: lowercased,
    /// characters outside `[a-z_]` stripped, leading/trailing `_` trimmed, and
    /// truncated to 63 chars. Schema names like `"Opportunity"` become
    /// `"opportunity"`; `"MySchema123"` becomes `"myschema"`.
    pub fn new(prefix: &str) -> Self {
        Self(prefix.create_type_id::<V7>())
    }

    /// Parses an `EntityId` from its string representation.
    ///
    /// Accepts any valid TypeID; the prefix is not constrained to a specific value.
    pub fn parse(s: &str) -> Result<Self, String> {
        let id = MagicTypeId::from_str(s).map_err(|e| format!("{e}"))?;
        Ok(Self(id))
    }

    /// Returns the TypeID prefix (e.g. `"project"`, `"opportunity"`).
    pub fn prefix(&self) -> &str {
        self.0.prefix().as_str()
    }

    /// Returns the string representation of this id.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
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
    fn new_uses_supplied_prefix() {
        let id = EntityId::new("project");
        assert!(
            id.as_str().starts_with("project_"),
            "expected 'project_' prefix, got: {id}"
        );
        assert_eq!(id.prefix(), "project");
    }

    #[test]
    fn new_sanitizes_prefix() {
        let id = EntityId::new("MySchema123");
        assert_eq!(id.prefix(), "myschema");
    }

    #[test]
    fn parse_valid() {
        let id = EntityId::new("contact");
        let parsed = EntityId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_accepts_any_prefix() {
        let legacy = EntityId::new("entity");
        let parsed = EntityId::parse(legacy.as_str()).unwrap();
        assert_eq!(parsed, legacy);

        let schema_prefixed = EntityId::new("opportunity");
        let parsed = EntityId::parse(schema_prefixed.as_str()).unwrap();
        assert_eq!(parsed, schema_prefixed);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(EntityId::parse("not-a-typeid").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let id = EntityId::new("tenant");
        let json = serde_json::to_string(&id).unwrap();
        let back: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_matches_as_str() {
        let id = EntityId::new("user");
        assert_eq!(id.to_string(), id.as_str());
    }
}
