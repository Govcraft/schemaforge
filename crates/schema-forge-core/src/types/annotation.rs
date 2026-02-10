use serde::{Deserialize, Serialize};

use super::field_name::FieldName;
use super::schema_version::SchemaVersion;

/// Annotations that can be applied to a schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "annotation")]
#[non_exhaustive]
pub enum Annotation {
    /// `@version(N)` -- declares the schema version.
    Version { version: SchemaVersion },
    /// `@display("field_name")` -- which field to use as display name.
    Display { field: FieldName },
}

impl std::fmt::Display for Annotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version { version } => write!(f, "@version({version})"),
            Self::Display { field } => write!(f, "@display(\"{field}\")"),
        }
    }
}

impl Annotation {
    /// Returns the annotation kind as a string, for dedup checking.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Version { .. } => "version",
            Self::Display { .. } => "display",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_version() {
        let a = Annotation::Version {
            version: SchemaVersion::new(3).unwrap(),
        };
        assert_eq!(a.to_string(), "@version(3)");
        assert_eq!(a.kind(), "version");
    }

    #[test]
    fn display_display() {
        let a = Annotation::Display {
            field: FieldName::new("name").unwrap(),
        };
        assert_eq!(a.to_string(), "@display(\"name\")");
        assert_eq!(a.kind(), "display");
    }

    #[test]
    fn serde_roundtrip() {
        let annotations = vec![
            Annotation::Version {
                version: SchemaVersion::new(2).unwrap(),
            },
            Annotation::Display {
                field: FieldName::new("title").unwrap(),
            },
        ];
        for a in annotations {
            let json = serde_json::to_string(&a).unwrap();
            let back: Annotation = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }
}
