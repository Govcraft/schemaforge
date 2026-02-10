use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// Optional constraints for `FieldType::Integer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct IntegerConstraints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

impl IntegerConstraints {
    /// Creates unconstrained integer.
    pub fn unconstrained() -> Self {
        Self {
            min: None,
            max: None,
        }
    }

    /// Creates integer with range bounds, validating min <= max.
    pub fn with_range(min: i64, max: i64) -> Result<Self, SchemaError> {
        if min > max {
            return Err(SchemaError::InvalidIntegerRange { min, max });
        }
        Ok(Self {
            min: Some(min),
            max: Some(max),
        })
    }

    /// Creates integer with only a minimum bound.
    pub fn with_min(min: i64) -> Self {
        Self {
            min: Some(min),
            max: None,
        }
    }

    /// Creates integer with only a maximum bound.
    pub fn with_max(max: i64) -> Self {
        Self {
            min: None,
            max: Some(max),
        }
    }

    /// Validates that min <= max if both are present.
    pub fn validate(&self) -> Result<(), SchemaError> {
        if let (Some(min), Some(max)) = (self.min, self.max) {
            if min > max {
                return Err(SchemaError::InvalidIntegerRange { min, max });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconstrained() {
        let c = IntegerConstraints::unconstrained();
        assert_eq!(c.min, None);
        assert_eq!(c.max, None);
    }

    #[test]
    fn valid_range() {
        let c = IntegerConstraints::with_range(0, 100).unwrap();
        assert_eq!(c.min, Some(0));
        assert_eq!(c.max, Some(100));
    }

    #[test]
    fn equal_range() {
        let c = IntegerConstraints::with_range(5, 5).unwrap();
        assert_eq!(c.min, Some(5));
        assert_eq!(c.max, Some(5));
    }

    #[test]
    fn invalid_range() {
        assert!(IntegerConstraints::with_range(10, 5).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let c = IntegerConstraints::with_range(0, 100).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back: IntegerConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn serde_skips_none() {
        let c = IntegerConstraints::unconstrained();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "{}");
    }
}
