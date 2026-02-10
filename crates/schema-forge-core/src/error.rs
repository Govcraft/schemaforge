use std::fmt;

/// Errors that occur when constructing or validating schema types.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaError {
    /// Schema name failed PascalCase validation.
    InvalidSchemaName(String),
    /// Field name failed snake_case validation.
    InvalidFieldName(String),
    /// Schema version must be >= 1.
    InvalidSchemaVersion(u32),
    /// Enum variants list was empty.
    EmptyEnumVariants,
    /// Enum variant string was empty.
    EmptyEnumVariant,
    /// Duplicate enum variant found.
    DuplicateEnumVariant(String),
    /// Integer constraint min > max.
    InvalidIntegerRange { min: i64, max: i64 },
    /// Float string could not be parsed.
    InvalidFloatString(String),
    /// Duplicate field name in a schema or composite.
    DuplicateFieldName(String),
    /// Duplicate annotation kind.
    DuplicateAnnotation(String),
    /// Schema definition has no fields.
    EmptyFields,
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSchemaName(s) => {
                write!(
                    f,
                    "invalid schema name '{s}': must be PascalCase [A-Z][a-zA-Z0-9]*"
                )
            }
            Self::InvalidFieldName(s) => {
                write!(
                    f,
                    "invalid field name '{s}': must be snake_case [a-z][a-z0-9_]*"
                )
            }
            Self::InvalidSchemaVersion(v) => {
                write!(f, "invalid schema version {v}: must be >= 1")
            }
            Self::EmptyEnumVariants => write!(f, "enum variants must not be empty"),
            Self::EmptyEnumVariant => write!(f, "enum variant must not be an empty string"),
            Self::DuplicateEnumVariant(v) => write!(f, "duplicate enum variant '{v}'"),
            Self::InvalidIntegerRange { min, max } => {
                write!(f, "invalid integer range: min ({min}) > max ({max})")
            }
            Self::InvalidFloatString(s) => {
                write!(f, "invalid float string '{s}': must be a valid f64")
            }
            Self::DuplicateFieldName(n) => write!(f, "duplicate field name '{n}'"),
            Self::DuplicateAnnotation(a) => write!(f, "duplicate annotation '{a}'"),
            Self::EmptyFields => write!(f, "schema must have at least one field"),
        }
    }
}

impl std::error::Error for SchemaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages() {
        let cases = vec![
            (
                SchemaError::InvalidSchemaName("foo".into()),
                "invalid schema name 'foo'",
            ),
            (
                SchemaError::InvalidFieldName("Foo".into()),
                "invalid field name 'Foo'",
            ),
            (
                SchemaError::InvalidSchemaVersion(0),
                "invalid schema version 0",
            ),
            (
                SchemaError::EmptyEnumVariants,
                "enum variants must not be empty",
            ),
            (
                SchemaError::EmptyEnumVariant,
                "enum variant must not be an empty string",
            ),
            (
                SchemaError::DuplicateEnumVariant("Active".into()),
                "duplicate enum variant 'Active'",
            ),
            (
                SchemaError::InvalidIntegerRange { min: 10, max: 5 },
                "invalid integer range: min (10) > max (5)",
            ),
            (
                SchemaError::InvalidFloatString("abc".into()),
                "invalid float string 'abc'",
            ),
            (
                SchemaError::DuplicateFieldName("name".into()),
                "duplicate field name 'name'",
            ),
            (
                SchemaError::DuplicateAnnotation("version".into()),
                "duplicate annotation 'version'",
            ),
            (
                SchemaError::EmptyFields,
                "schema must have at least one field",
            ),
        ];

        for (error, expected_prefix) in cases {
            let msg = error.to_string();
            assert!(
                msg.starts_with(expected_prefix),
                "Error display for {error:?} = '{msg}', expected to start with '{expected_prefix}'"
            );
        }
    }

    #[test]
    fn error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(SchemaError::InvalidSchemaName("x".into()));
        assert!(err.to_string().contains("invalid schema name"));
    }
}
