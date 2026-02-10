use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// A validated PascalCase schema name matching `[A-Z][a-zA-Z0-9]*`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SchemaName(String);

impl SchemaName {
    /// Creates a new `SchemaName`, validating PascalCase format.
    pub fn new(s: impl Into<String>) -> Result<Self, SchemaError> {
        let s = s.into();
        if !is_pascal_case(&s) {
            return Err(SchemaError::InvalidSchemaName(s));
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
        for name in ["", "contact", "my_schema", "123Schema", "My Schema", "my-schema"] {
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
}
