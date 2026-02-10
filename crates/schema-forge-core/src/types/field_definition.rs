use serde::{Deserialize, Serialize};

use super::field_modifier::FieldModifier;
use super::field_name::FieldName;
use super::field_type::FieldType;

/// A complete field definition: name, type, and modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: FieldName,
    pub field_type: FieldType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<FieldModifier>,
}

impl FieldDefinition {
    /// Creates a new field definition with no modifiers.
    pub fn new(name: FieldName, field_type: FieldType) -> Self {
        Self {
            name,
            field_type,
            modifiers: Vec::new(),
        }
    }

    /// Creates a new field definition with modifiers.
    pub fn with_modifiers(
        name: FieldName,
        field_type: FieldType,
        modifiers: Vec<FieldModifier>,
    ) -> Self {
        Self {
            name,
            field_type,
            modifiers,
        }
    }

    /// Returns true if this field has the `Required` modifier.
    pub fn is_required(&self) -> bool {
        self.modifiers
            .iter()
            .any(|m| matches!(m, FieldModifier::Required))
    }

    /// Returns true if this field has the `Indexed` modifier.
    pub fn is_indexed(&self) -> bool {
        self.modifiers
            .iter()
            .any(|m| matches!(m, FieldModifier::Indexed))
    }
}

impl std::fmt::Display for FieldDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.field_type)?;
        for m in &self.modifiers {
            write!(f, " @{m}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::default_value::DefaultValue;
    use crate::types::text_constraints::TextConstraints;

    #[test]
    fn new_field() {
        let fd = FieldDefinition::new(
            FieldName::new("email").unwrap(),
            FieldType::Text(TextConstraints::with_max_length(255)),
        );
        assert_eq!(fd.name.as_str(), "email");
        assert!(!fd.is_required());
        assert!(!fd.is_indexed());
        assert!(fd.modifiers.is_empty());
    }

    #[test]
    fn with_modifiers() {
        let fd = FieldDefinition::with_modifiers(
            FieldName::new("email").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required, FieldModifier::Indexed],
        );
        assert!(fd.is_required());
        assert!(fd.is_indexed());
    }

    #[test]
    fn display() {
        let fd = FieldDefinition::with_modifiers(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
        );
        assert_eq!(fd.to_string(), "name: Text @required");
    }

    #[test]
    fn serde_roundtrip() {
        let fd = FieldDefinition::with_modifiers(
            FieldName::new("active").unwrap(),
            FieldType::Boolean,
            vec![FieldModifier::Default {
                value: DefaultValue::Boolean(true),
            }],
        );
        let json = serde_json::to_string(&fd).unwrap();
        let back: FieldDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(fd, back);
    }

    #[test]
    fn serde_skips_empty_modifiers() {
        let fd = FieldDefinition::new(FieldName::new("x").unwrap(), FieldType::Boolean);
        let json = serde_json::to_string(&fd).unwrap();
        assert!(!json.contains("modifiers"));
    }
}
