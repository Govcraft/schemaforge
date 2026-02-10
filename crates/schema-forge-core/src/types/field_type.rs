use serde::{Deserialize, Serialize};

use super::cardinality::Cardinality;
use super::enum_variants::EnumVariants;
use super::field_definition::FieldDefinition;
use super::float_constraints::FloatConstraints;
use super::integer_constraints::IntegerConstraints;
use super::schema_name::SchemaName;
use super::text_constraints::TextConstraints;

/// The core DSL type system for schema fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[non_exhaustive]
pub enum FieldType {
    Text(TextConstraints),
    RichText,
    Integer(IntegerConstraints),
    Float(FloatConstraints),
    Boolean,
    DateTime,
    Enum(EnumVariants),
    Json,
    Relation {
        target: SchemaName,
        cardinality: Cardinality,
    },
    Array(Box<FieldType>),
    Composite(Vec<FieldDefinition>),
}

impl std::fmt::Display for FieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(_) => write!(f, "Text"),
            Self::RichText => write!(f, "RichText"),
            Self::Integer(_) => write!(f, "Integer"),
            Self::Float(_) => write!(f, "Float"),
            Self::Boolean => write!(f, "Boolean"),
            Self::DateTime => write!(f, "DateTime"),
            Self::Enum(v) => write!(f, "Enum{v}"),
            Self::Json => write!(f, "Json"),
            Self::Relation {
                target,
                cardinality,
            } => write!(f, "Relation({target}, {cardinality})"),
            Self::Array(inner) => write!(f, "Array<{inner}>"),
            Self::Composite(fields) => write!(f, "Composite({} fields)", fields.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_simple_types() {
        assert_eq!(FieldType::Boolean.to_string(), "Boolean");
        assert_eq!(FieldType::DateTime.to_string(), "DateTime");
        assert_eq!(FieldType::RichText.to_string(), "RichText");
        assert_eq!(FieldType::Json.to_string(), "Json");
    }

    #[test]
    fn display_text() {
        let t = FieldType::Text(TextConstraints::with_max_length(255));
        assert_eq!(t.to_string(), "Text");
    }

    #[test]
    fn display_relation() {
        let t = FieldType::Relation {
            target: SchemaName::new("Company").unwrap(),
            cardinality: Cardinality::One,
        };
        assert_eq!(t.to_string(), "Relation(Company, One)");
    }

    #[test]
    fn display_array() {
        let t = FieldType::Array(Box::new(FieldType::Boolean));
        assert_eq!(t.to_string(), "Array<Boolean>");
    }

    #[test]
    fn serde_roundtrip_simple() {
        for ft in [
            FieldType::Boolean,
            FieldType::DateTime,
            FieldType::RichText,
            FieldType::Json,
        ] {
            let json = serde_json::to_string(&ft).unwrap();
            let back: FieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(ft, back);
        }
    }

    #[test]
    fn serde_roundtrip_text() {
        let ft = FieldType::Text(TextConstraints::with_max_length(100));
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn serde_roundtrip_relation() {
        let ft = FieldType::Relation {
            target: SchemaName::new("Contact").unwrap(),
            cardinality: Cardinality::Many,
        };
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn serde_roundtrip_array() {
        let ft = FieldType::Array(Box::new(FieldType::Integer(
            IntegerConstraints::unconstrained(),
        )));
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn serde_roundtrip_enum() {
        let ft = FieldType::Enum(EnumVariants::new(vec!["A".into(), "B".into()]).unwrap());
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }
}
