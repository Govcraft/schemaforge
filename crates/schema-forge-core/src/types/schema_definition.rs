use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

use super::annotation::Annotation;
use super::field_definition::FieldDefinition;
use super::schema_id::SchemaId;
use super::schema_name::SchemaName;

/// A complete schema definition: id, name, fields, and annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDefinition {
    pub id: SchemaId,
    pub name: SchemaName,
    pub fields: Vec<FieldDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
}

impl SchemaDefinition {
    /// Creates a new `SchemaDefinition`, validating:
    /// - fields is non-empty
    /// - no duplicate field names
    /// - no duplicate annotation kinds
    pub fn new(
        id: SchemaId,
        name: SchemaName,
        fields: Vec<FieldDefinition>,
        annotations: Vec<Annotation>,
    ) -> Result<Self, SchemaError> {
        if fields.is_empty() {
            return Err(SchemaError::EmptyFields);
        }

        let mut field_names = HashSet::with_capacity(fields.len());
        for f in &fields {
            if !field_names.insert(f.name.as_str()) {
                return Err(SchemaError::DuplicateFieldName(f.name.to_string()));
            }
        }

        let mut ann_kinds = HashSet::with_capacity(annotations.len());
        for a in &annotations {
            if !ann_kinds.insert(a.kind()) {
                return Err(SchemaError::DuplicateAnnotation(a.kind().to_string()));
            }
        }

        Ok(Self {
            id,
            name,
            fields,
            annotations,
        })
    }

    /// Looks up a field by name.
    pub fn field(&self, name: &str) -> Option<&FieldDefinition> {
        self.fields.iter().find(|f| f.name.as_str() == name)
    }

    /// Returns the `@access` annotation if present.
    pub fn access_annotation(&self) -> Option<&Annotation> {
        self.annotations
            .iter()
            .find(|a| matches!(a, Annotation::Access { .. }))
    }

    /// Returns true if this schema has `@access` restrictions.
    pub fn has_access_restrictions(&self) -> bool {
        self.access_annotation().is_some()
    }

    /// Returns true if this schema has the `@system` annotation.
    pub fn is_system(&self) -> bool {
        self.annotations
            .iter()
            .any(|a| matches!(a, Annotation::System))
    }
}

impl std::fmt::Display for SchemaDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for a in &self.annotations {
            writeln!(f, "{a}")?;
        }
        writeln!(f, "schema {} {{", self.name)?;
        for field in &self.fields {
            writeln!(f, "  {field}")?;
        }
        write!(f, "}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::field_modifier::FieldModifier;
    use crate::types::field_name::FieldName;
    use crate::types::field_type::FieldType;
    use crate::types::schema_version::SchemaVersion;
    use crate::types::text_constraints::TextConstraints;

    fn make_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    #[test]
    fn valid_schema() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![make_field("name"), make_field("email")],
            vec![Annotation::Version {
                version: SchemaVersion::new(1).unwrap(),
            }],
        )
        .unwrap();
        assert_eq!(sd.name.as_str(), "Contact");
        assert_eq!(sd.fields.len(), 2);
        assert!(sd.field("name").is_some());
        assert!(sd.field("missing").is_none());
    }

    #[test]
    fn empty_fields() {
        let result = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Empty").unwrap(),
            vec![],
            vec![],
        );
        assert!(matches!(result, Err(SchemaError::EmptyFields)));
    }

    #[test]
    fn duplicate_field_names() {
        let result = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Dup").unwrap(),
            vec![make_field("name"), make_field("name")],
            vec![],
        );
        assert!(matches!(result, Err(SchemaError::DuplicateFieldName(_))));
    }

    #[test]
    fn duplicate_annotations() {
        let result = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Dup").unwrap(),
            vec![make_field("name")],
            vec![
                Annotation::Version {
                    version: SchemaVersion::new(1).unwrap(),
                },
                Annotation::Version {
                    version: SchemaVersion::new(2).unwrap(),
                },
            ],
        );
        assert!(matches!(result, Err(SchemaError::DuplicateAnnotation(_))));
    }

    #[test]
    fn display() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            )],
            vec![Annotation::Version {
                version: SchemaVersion::new(1).unwrap(),
            }],
        )
        .unwrap();
        let s = sd.to_string();
        assert!(s.contains("@version(1)"));
        assert!(s.contains("schema Contact {"));
        assert!(s.contains("name: Text @required"));
    }

    #[test]
    fn serde_roundtrip() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Task").unwrap(),
            vec![
                make_field("title"),
                FieldDefinition::new(FieldName::new("done").unwrap(), FieldType::Boolean),
            ],
            vec![],
        )
        .unwrap();
        let json = serde_json::to_string(&sd).unwrap();
        let back: SchemaDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(sd, back);
    }

    #[test]
    fn access_annotation_returns_some_when_present() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Secured").unwrap(),
            vec![make_field("name")],
            vec![Annotation::Access {
                read: vec!["viewer".into()],
                write: vec!["editor".into()],
                delete: vec!["admin".into()],
                cross_tenant_read: vec![],
            }],
        )
        .unwrap();
        assert!(sd.access_annotation().is_some());
        assert!(matches!(
            sd.access_annotation(),
            Some(Annotation::Access { .. })
        ));
    }

    #[test]
    fn access_annotation_returns_none_when_absent() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Open").unwrap(),
            vec![make_field("name")],
            vec![],
        )
        .unwrap();
        assert!(sd.access_annotation().is_none());
    }

    #[test]
    fn has_access_restrictions_true() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Secured").unwrap(),
            vec![make_field("name")],
            vec![Annotation::Access {
                read: vec!["viewer".into()],
                write: vec![],
                delete: vec![],
                cross_tenant_read: vec![],
            }],
        )
        .unwrap();
        assert!(sd.has_access_restrictions());
    }

    #[test]
    fn has_access_restrictions_false() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Open").unwrap(),
            vec![make_field("name")],
            vec![],
        )
        .unwrap();
        assert!(!sd.has_access_restrictions());
    }

    #[test]
    fn is_system_true() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Internal").unwrap(),
            vec![make_field("name")],
            vec![Annotation::System],
        )
        .unwrap();
        assert!(sd.is_system());
    }

    #[test]
    fn is_system_false() {
        let sd = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Regular").unwrap(),
            vec![make_field("name")],
            vec![],
        )
        .unwrap();
        assert!(!sd.is_system());
    }
}
