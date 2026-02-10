use serde::{Deserialize, Serialize};

use super::default_value::DefaultValue;

/// Modifiers that can be applied to a field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "modifier")]
#[non_exhaustive]
pub enum FieldModifier {
    Required,
    Indexed,
    Default { value: DefaultValue },
}

impl std::fmt::Display for FieldModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Required => write!(f, "required"),
            Self::Indexed => write!(f, "indexed"),
            Self::Default { value } => write!(f, "default({value})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(FieldModifier::Required.to_string(), "required");
        assert_eq!(FieldModifier::Indexed.to_string(), "indexed");
        assert_eq!(
            FieldModifier::Default {
                value: DefaultValue::Integer(42)
            }
            .to_string(),
            "default(42)"
        );
    }

    #[test]
    fn serde_roundtrip() {
        let modifiers = vec![
            FieldModifier::Required,
            FieldModifier::Indexed,
            FieldModifier::Default {
                value: DefaultValue::Boolean(true),
            },
        ];
        for m in modifiers {
            let json = serde_json::to_string(&m).unwrap();
            let back: FieldModifier = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }
}
