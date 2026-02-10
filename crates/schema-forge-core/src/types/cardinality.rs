use std::fmt;

use serde::{Deserialize, Serialize};

/// Cardinality of a relation field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Cardinality {
    One,
    Many,
}

impl fmt::Display for Cardinality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::One => write!(f, "One"),
            Self::Many => write!(f, "Many"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(Cardinality::One.to_string(), "One");
        assert_eq!(Cardinality::Many.to_string(), "Many");
    }

    #[test]
    fn serde_roundtrip() {
        for c in [Cardinality::One, Cardinality::Many] {
            let json = serde_json::to_string(&c).unwrap();
            let back: Cardinality = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }
}
