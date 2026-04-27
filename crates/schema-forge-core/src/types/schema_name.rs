use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;
use crate::types::cedar_reserved::reserved_schema_name;

/// A validated PascalCase schema name matching `[A-Z][a-zA-Z0-9]*`.
///
/// Names that collide with SchemaForge's Cedar policy namespace are also
/// rejected; see [`crate::types::cedar_reserved::RESERVED_SCHEMA_NAMES`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SchemaName(String);

impl SchemaName {
    /// Creates a new `SchemaName`, validating PascalCase format and rejecting
    /// names reserved by the Cedar policy generator.
    pub fn new(s: impl Into<String>) -> Result<Self, SchemaError> {
        let s = s.into();
        if !is_pascal_case(&s) {
            return Err(SchemaError::InvalidSchemaName(s));
        }
        if reserved_schema_name(&s).is_some() {
            return Err(SchemaError::ReservedSchemaName(s));
        }
        Ok(Self(s))
    }

    /// Returns the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_pascal_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric())
}

impl fmt::Display for SchemaName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<SchemaName> for String {
    fn from(n: SchemaName) -> String {
        n.0
    }
}

impl TryFrom<String> for SchemaName {
    type Error = SchemaError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl AsRef<str> for SchemaName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for name in ["Contact", "A", "MySchema123", "CRM"] {
            assert!(SchemaName::new(name).is_ok(), "expected valid: {name}");
        }
    }

    #[test]
    fn invalid_names() {
        for name in [
            "",
            "contact",
            "my_schema",
            "123Schema",
            "My Schema",
            "my-schema",
        ] {
            assert!(SchemaName::new(name).is_err(), "expected invalid: {name}");
        }
    }

    #[test]
    fn display_roundtrip() {
        let name = SchemaName::new("Contact").unwrap();
        assert_eq!(name.to_string(), "Contact");
        assert_eq!(name.as_str(), "Contact");
    }

    #[test]
    fn serde_roundtrip() {
        let name = SchemaName::new("Contact").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"Contact\"");
        let back: SchemaName = serde_json::from_str(&json).unwrap();
        assert_eq!(name, back);
    }

    #[test]
    fn serde_rejects_invalid() {
        let result = serde_json::from_str::<SchemaName>("\"bad_name\"");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_cedar_namespace_collisions() {
        for name in ["Forge", "SchemaForge", "Principal", "Cedar"] {
            let err = SchemaName::new(name).unwrap_err();
            assert!(
                matches!(err, SchemaError::ReservedSchemaName(ref s) if s == name),
                "expected ReservedSchemaName for {name}, got {err:?}"
            );
        }
    }

    #[test]
    fn permits_app_names_that_resemble_cedar_types() {
        // `User`, `Group`, `Schema`, `Action` are entity types in Cedar but
        // SchemaForge namespaces its own types under `Forge::`, so app
        // schemas may use these names freely.
        for name in ["User", "Group", "Schema", "Action"] {
            assert!(
                SchemaName::new(name).is_ok(),
                "should permit app schema name {name}"
            );
        }
    }
}
