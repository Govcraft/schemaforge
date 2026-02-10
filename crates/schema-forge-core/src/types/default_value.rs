use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// A default value for a field. Float values are stored as strings
/// to preserve `Eq`/`Hash` and round-trip fidelity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum DefaultValue {
    String(String),
    Integer(i64),
    /// Float stored as a string representation (e.g. `"3.14"`).
    Float(String),
    Boolean(bool),
}

impl DefaultValue {
    /// Creates a `Float` variant after validating the string parses as f64.
    pub fn float(s: impl Into<String>) -> Result<Self, SchemaError> {
        let s = s.into();
        s.parse::<f64>()
            .map_err(|_| SchemaError::InvalidFloatString(s.clone()))?;
        Ok(Self::Float(s))
    }

    /// For `Float` variants, parses and returns the f64 value.
    /// Returns `None` for other variants.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(s) => s.parse().ok(),
            _ => None,
        }
    }
}

impl fmt::Display for DefaultValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Integer(i) => write!(f, "{i}"),
            Self::Float(s) => write!(f, "{s}"),
            Self::Boolean(b) => write!(f, "{b}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_valid() {
        let dv = DefaultValue::float("2.72").unwrap();
        assert_eq!(dv.as_f64(), Some(2.72));
        assert_eq!(dv.to_string(), "2.72");
    }

    #[test]
    fn float_invalid() {
        assert!(DefaultValue::float("abc").is_err());
    }

    #[test]
    fn display_variants() {
        assert_eq!(DefaultValue::String("hello".into()).to_string(), "\"hello\"");
        assert_eq!(DefaultValue::Integer(42).to_string(), "42");
        assert_eq!(DefaultValue::Boolean(true).to_string(), "true");
    }

    #[test]
    fn as_f64_non_float() {
        assert_eq!(DefaultValue::Integer(1).as_f64(), None);
    }

    #[test]
    fn serde_roundtrip() {
        let values = vec![
            DefaultValue::String("hello".into()),
            DefaultValue::Integer(42),
            DefaultValue::float("3.14").unwrap(),
            DefaultValue::Boolean(false),
        ];
        for v in values {
            let json = serde_json::to_string(&v).unwrap();
            let back: DefaultValue = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }
}
