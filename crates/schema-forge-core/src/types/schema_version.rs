use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// A positive schema version number (>= 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// Creates a new `SchemaVersion`, returning an error if `v` is 0.
    pub fn new(v: u32) -> Result<Self, SchemaError> {
        if v == 0 {
            return Err(SchemaError::InvalidSchemaVersion(v));
        }
        Ok(Self(v))
    }

    /// Returns the inner version number.
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for SchemaVersion {
    fn default() -> Self {
        Self(1)
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<SchemaVersion> for u32 {
    fn from(v: SchemaVersion) -> u32 {
        v.0
    }
}

impl TryFrom<u32> for SchemaVersion {
    type Error = SchemaError;

    fn try_from(v: u32) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_version() {
        let v = SchemaVersion::new(1).unwrap();
        assert_eq!(v.get(), 1);
        assert_eq!(v.to_string(), "1");
    }

    #[test]
    fn zero_is_invalid() {
        assert!(SchemaVersion::new(0).is_err());
    }

    #[test]
    fn default_is_one() {
        assert_eq!(SchemaVersion::default().get(), 1);
    }

    #[test]
    fn serde_roundtrip() {
        let v = SchemaVersion::new(5).unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "5");
        let back: SchemaVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn serde_rejects_zero() {
        let result = serde_json::from_str::<SchemaVersion>("0");
        assert!(result.is_err());
    }

    #[test]
    fn ordering() {
        let v1 = SchemaVersion::new(1).unwrap();
        let v3 = SchemaVersion::new(3).unwrap();
        assert!(v1 < v3);
    }
}
