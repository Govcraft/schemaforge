use serde::{Deserialize, Serialize};

/// Optional constraints for `FieldType::Text`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TextConstraints {
    /// Maximum character length, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
}

impl TextConstraints {
    /// Creates unconstrained text (no max length).
    pub fn unconstrained() -> Self {
        Self { max_length: None }
    }

    /// Creates text with a max length.
    pub fn with_max_length(max: u32) -> Self {
        Self {
            max_length: Some(max),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconstrained() {
        let c = TextConstraints::unconstrained();
        assert_eq!(c.max_length, None);
    }

    #[test]
    fn with_max() {
        let c = TextConstraints::with_max_length(255);
        assert_eq!(c.max_length, Some(255));
    }

    #[test]
    fn default_is_unconstrained() {
        assert_eq!(TextConstraints::default(), TextConstraints::unconstrained());
    }

    #[test]
    fn serde_roundtrip() {
        let c = TextConstraints::with_max_length(100);
        let json = serde_json::to_string(&c).unwrap();
        let back: TextConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn serde_skips_none() {
        let c = TextConstraints::unconstrained();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "{}");
    }
}
