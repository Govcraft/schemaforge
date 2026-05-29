use serde::{Deserialize, Serialize};

/// Optional constraints for `FieldType::Bytes`.
///
/// A `bytes` field stores inline binary (hashes, signatures, key material,
/// nonces). `max_size`, when set, caps the byte length accepted on write; it is
/// enforced fail-closed at the API boundary and by the storage backends, never
/// silently truncated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct BytesConstraints {
    /// Maximum byte length, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size: Option<usize>,
}

impl BytesConstraints {
    /// Creates unconstrained bytes (no maximum size).
    #[must_use]
    pub fn unconstrained() -> Self {
        Self { max_size: None }
    }

    /// Creates bytes with a maximum byte length.
    #[must_use]
    pub fn with_max_size(max: usize) -> Self {
        Self {
            max_size: Some(max),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconstrained() {
        let c = BytesConstraints::unconstrained();
        assert_eq!(c.max_size, None);
    }

    #[test]
    fn with_max() {
        let c = BytesConstraints::with_max_size(1024);
        assert_eq!(c.max_size, Some(1024));
    }

    #[test]
    fn default_is_unconstrained() {
        assert_eq!(
            BytesConstraints::default(),
            BytesConstraints::unconstrained()
        );
    }

    #[test]
    fn serde_roundtrip() {
        let c = BytesConstraints::with_max_size(64);
        let json = serde_json::to_string(&c).unwrap();
        let back: BytesConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn serde_skips_none() {
        let c = BytesConstraints::unconstrained();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "{}");
    }
}
