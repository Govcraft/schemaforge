use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// A validated snake_case field name matching `[a-z][a-z0-9_]*`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FieldName(String);

impl FieldName {
    /// Creates a new `FieldName`, validating snake_case format.
    pub fn new(s: impl Into<String>) -> Result<Self, SchemaError> {
        let s = s.into();
        if !is_snake_case(&s) {
            return Err(SchemaError::InvalidFieldName(s));
        }
        Ok(Self(s))
    }

    /// Returns the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_snake_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl fmt::Display for FieldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<FieldName> for String {
    fn from(n: FieldName) -> String {
        n.0
    }
}

impl TryFrom<String> for FieldName {
    type Error = SchemaError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl AsRef<str> for FieldName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for name in ["name", "first_name", "a", "field123", "my_field_2"] {
            assert!(FieldName::new(name).is_ok(), "expected valid: {name}");
        }
    }

    #[test]
    fn invalid_names() {
        for name in ["", "Name", "UPPER", "123field", "_leading", "has-dash", "has space"] {
            assert!(FieldName::new(name).is_err(), "expected invalid: {name}");
        }
    }

    #[test]
    fn display_roundtrip() {
        let name = FieldName::new("first_name").unwrap();
        assert_eq!(name.to_string(), "first_name");
        assert_eq!(name.as_str(), "first_name");
    }

    #[test]
    fn serde_roundtrip() {
        let name = FieldName::new("email").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"email\"");
        let back: FieldName = serde_json::from_str(&json).unwrap();
        assert_eq!(name, back);
    }

    #[test]
    fn serde_rejects_invalid() {
        let result = serde_json::from_str::<FieldName>("\"BadName\"");
        assert!(result.is_err());
    }
}
