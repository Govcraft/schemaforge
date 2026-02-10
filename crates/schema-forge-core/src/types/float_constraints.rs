use serde::{Deserialize, Serialize};

/// Optional constraints for `FieldType::Float`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FloatConstraints {
    /// Decimal precision (number of decimal places), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<u32>,
}

impl FloatConstraints {
    /// Creates unconstrained float.
    pub fn unconstrained() -> Self {
        Self { precision: None }
    }

    /// Creates float with a precision constraint.
    pub fn with_precision(precision: u32) -> Self {
        Self {
            precision: Some(precision),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconstrained() {
        let c = FloatConstraints::unconstrained();
        assert_eq!(c.precision, None);
    }

    #[test]
    fn with_precision() {
        let c = FloatConstraints::with_precision(2);
        assert_eq!(c.precision, Some(2));
    }

    #[test]
    fn serde_roundtrip() {
        let c = FloatConstraints::with_precision(4);
        let json = serde_json::to_string(&c).unwrap();
        let back: FloatConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
