use serde::{Deserialize, Serialize};

use super::field_annotation::FieldAnnotation;
use super::field_modifier::FieldModifier;
use super::field_name::FieldName;
use super::field_type::FieldType;

/// A complete field definition: name, type, modifiers, and annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: FieldName,
    pub field_type: FieldType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<FieldModifier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<FieldAnnotation>,
}

impl FieldDefinition {
    /// Creates a new field definition with no modifiers or annotations.
    pub fn new(name: FieldName, field_type: FieldType) -> Self {
        Self {
            name,
            field_type,
            modifiers: Vec::new(),
            annotations: Vec::new(),
        }
    }

    /// Creates a new field definition with modifiers but no annotations.
    pub fn with_modifiers(
        name: FieldName,
        field_type: FieldType,
        modifiers: Vec<FieldModifier>,
    ) -> Self {
        Self {
            name,
            field_type,
            modifiers,
            annotations: Vec::new(),
        }
    }

    /// Creates a new field definition with modifiers and annotations.
    pub fn with_annotations(
        name: FieldName,
        field_type: FieldType,
        modifiers: Vec<FieldModifier>,
        annotations: Vec<FieldAnnotation>,
    ) -> Self {
        Self {
            name,
            field_type,
            modifiers,
            annotations,
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

    /// Returns true if this field has the `Owner` annotation.
    pub fn has_owner(&self) -> bool {
        self.annotations
            .iter()
            .any(|a| matches!(a, FieldAnnotation::Owner))
    }

    /// Returns the format hint string if this field has a `@format` annotation.
    pub fn format_hint(&self) -> Option<&str> {
        self.annotations.iter().find_map(|a| match a {
            FieldAnnotation::Format { format_type } => Some(format_type.as_str()),
            _ => None,
        })
    }

    /// Returns the `FieldAccess` annotation if present.
    pub fn field_access(&self) -> Option<&FieldAnnotation> {
        self.annotations
            .iter()
            .find(|a| matches!(a, FieldAnnotation::FieldAccess { .. }))
    }
}

impl std::fmt::Display for FieldDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.field_type)?;
        for m in &self.modifiers {
            write!(f, " @{m}")?;
        }
        for a in &self.annotations {
            write!(f, " {a}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::default_value::DefaultValue;
    use crate::types::integer_constraints::IntegerConstraints;
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

    #[test]
    fn with_annotations_constructor() {
        let fd = FieldDefinition::with_annotations(
            FieldName::new("owner_id").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
            vec![FieldAnnotation::Owner],
        );
        assert!(fd.is_required());
        assert!(fd.has_owner());
        assert_eq!(fd.annotations.len(), 1);
    }

    #[test]
    fn has_owner_true() {
        let fd = FieldDefinition::with_annotations(
            FieldName::new("user_id").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::Owner],
        );
        assert!(fd.has_owner());
    }

    #[test]
    fn has_owner_false() {
        let fd = FieldDefinition::new(
            FieldName::new("user_id").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        assert!(!fd.has_owner());
    }

    #[test]
    fn field_access_some() {
        let fd = FieldDefinition::with_annotations(
            FieldName::new("salary").unwrap(),
            FieldType::Integer(IntegerConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::FieldAccess {
                read: vec!["hr".into()],
                write: vec!["hr".into()],
            }],
        );
        assert!(fd.field_access().is_some());
    }

    #[test]
    fn field_access_none() {
        let fd = FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        assert!(fd.field_access().is_none());
    }

    #[test]
    fn display_with_annotations() {
        let fd = FieldDefinition::with_annotations(
            FieldName::new("owner_id").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
            vec![FieldAnnotation::Owner],
        );
        assert_eq!(fd.to_string(), "owner_id: Text @required @owner");
    }

    #[test]
    fn serde_roundtrip_with_annotations() {
        let fd = FieldDefinition::with_annotations(
            FieldName::new("salary").unwrap(),
            FieldType::Integer(IntegerConstraints::unconstrained()),
            vec![FieldModifier::Required],
            vec![FieldAnnotation::FieldAccess {
                read: vec!["hr".into()],
                write: vec!["hr".into()],
            }],
        );
        let json = serde_json::to_string(&fd).unwrap();
        let back: FieldDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(fd, back);
    }

    #[test]
    fn format_hint_some() {
        let fd = FieldDefinition::with_annotations(
            FieldName::new("price").unwrap(),
            FieldType::Float(crate::types::float_constraints::FloatConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::Format {
                format_type: "currency".into(),
            }],
        );
        assert_eq!(fd.format_hint(), Some("currency"));
    }

    #[test]
    fn format_hint_none() {
        let fd = FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        assert_eq!(fd.format_hint(), None);
    }

    #[test]
    fn serde_skips_empty_annotations() {
        let fd = FieldDefinition::new(FieldName::new("x").unwrap(), FieldType::Boolean);
        let json = serde_json::to_string(&fd).unwrap();
        assert!(!json.contains("annotations"));
    }
}
