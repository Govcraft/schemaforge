use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::SchemaError;

/// A non-empty, deduplicated list of enum variant strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumVariants(Vec<String>);

impl EnumVariants {
    /// Creates a new `EnumVariants`, validating:
    /// - list is non-empty
    /// - no empty strings
    /// - no duplicates
    pub fn new(variants: Vec<String>) -> Result<Self, SchemaError> {
        if variants.is_empty() {
            return Err(SchemaError::EmptyEnumVariants);
        }
        let mut seen = HashSet::with_capacity(variants.len());
        for v in &variants {
            if v.is_empty() {
                return Err(SchemaError::EmptyEnumVariant);
            }
            if !seen.insert(v.as_str()) {
                return Err(SchemaError::DuplicateEnumVariant(v.clone()));
            }
        }
        Ok(Self(variants))
    }

    /// Returns the variants as a slice.
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Returns the number of variants.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always returns false (guaranteed non-empty by construction).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Returns an iterator over the variants.
    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.0.iter()
    }
}

impl fmt::Display for EnumVariants {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.0.join(", "))
    }
}

impl Serialize for EnumVariants {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EnumVariants {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let variants = Vec::<String>::deserialize(deserializer)?;
        Self::new(variants).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_variants() {
        let v = EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap();
        assert_eq!(v.len(), 2);
        assert!(!v.is_empty());
        assert_eq!(v.as_slice(), &["Active", "Inactive"]);
    }

    #[test]
    fn empty_list() {
        assert!(matches!(
            EnumVariants::new(vec![]),
            Err(SchemaError::EmptyEnumVariants)
        ));
    }

    #[test]
    fn empty_variant_string() {
        assert!(matches!(
            EnumVariants::new(vec!["Good".into(), "".into()]),
            Err(SchemaError::EmptyEnumVariant)
        ));
    }

    #[test]
    fn duplicate_variant() {
        assert!(matches!(
            EnumVariants::new(vec!["A".into(), "B".into(), "A".into()]),
            Err(SchemaError::DuplicateEnumVariant(_))
        ));
    }

    #[test]
    fn display() {
        let v = EnumVariants::new(vec!["A".into(), "B".into()]).unwrap();
        assert_eq!(v.to_string(), "[A, B]");
    }

    #[test]
    fn serde_roundtrip() {
        let v = EnumVariants::new(vec!["Active".into(), "Pending".into()]).unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#"["Active","Pending"]"#);
        let back: EnumVariants = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_rejects_empty() {
        let result = serde_json::from_str::<EnumVariants>("[]");
        assert!(result.is_err());
    }
}
