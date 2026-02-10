use std::fmt;

use schema_forge_core::error::SchemaError;

/// A byte-offset span in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl Span {
    /// Creates a new span from start (inclusive) to end (exclusive).
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// Errors that occur during DSL parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DslError {
    /// The lexer encountered a token that does not match any rule.
    InvalidToken { span: Span },

    /// The parser encountered an unexpected token.
    UnexpectedToken {
        expected: String,
        found: String,
        span: Span,
    },

    /// The parser reached the end of input when more tokens were expected.
    UnexpectedEndOfInput { expected: String },

    /// A schema name failed PascalCase validation.
    InvalidSchemaName { name: String, span: Span },

    /// A field name failed snake_case validation.
    InvalidFieldName { name: String, span: Span },

    /// A duplicate field name was found within a schema or composite.
    DuplicateFieldName { name: String, span: Span },

    /// A duplicate annotation kind was found on a schema.
    DuplicateAnnotation { kind: String, span: Span },

    /// A schema declaration has no fields.
    EmptySchema { name: String, span: Span },

    /// An integer literal could not be parsed.
    InvalidIntegerLiteral { text: String, span: Span },

    /// A float literal could not be parsed.
    InvalidFloatLiteral { text: String, span: Span },

    /// An error propagated from schema-forge-core validation.
    CoreSchemaError { source: SchemaError, span: Span },

    /// Enum variant list was empty.
    EmptyEnumVariants { span: Span },

    /// Duplicate enum variant found.
    DuplicateEnumVariant { variant: String, span: Span },

    /// Integer constraint min > max.
    InvalidIntegerRange { min: i64, max: i64, span: Span },
}

impl fmt::Display for DslError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken { span } => {
                write!(f, "invalid token at {span}")
            }
            Self::UnexpectedToken {
                expected,
                found,
                span,
            } => {
                write!(
                    f,
                    "unexpected token at {span}: expected {expected}, found {found}"
                )
            }
            Self::UnexpectedEndOfInput { expected } => {
                write!(f, "unexpected end of input: expected {expected}")
            }
            Self::InvalidSchemaName { name, span } => {
                write!(
                    f,
                    "invalid schema name '{name}' at {span}: must be PascalCase [A-Z][a-zA-Z0-9]*"
                )
            }
            Self::InvalidFieldName { name, span } => {
                write!(
                    f,
                    "invalid field name '{name}' at {span}: must be snake_case [a-z][a-z0-9_]*"
                )
            }
            Self::DuplicateFieldName { name, span } => {
                write!(f, "duplicate field name '{name}' at {span}")
            }
            Self::DuplicateAnnotation { kind, span } => {
                write!(f, "duplicate annotation '@{kind}' at {span}")
            }
            Self::EmptySchema { name, span } => {
                write!(
                    f,
                    "schema '{name}' at {span} has no fields; add at least one field"
                )
            }
            Self::InvalidIntegerLiteral { text, span } => {
                write!(
                    f,
                    "invalid integer literal '{text}' at {span}: expected a valid integer"
                )
            }
            Self::InvalidFloatLiteral { text, span } => {
                write!(
                    f,
                    "invalid float literal '{text}' at {span}: expected a valid number"
                )
            }
            Self::CoreSchemaError { source, span } => {
                write!(f, "schema validation error at {span}: {source}")
            }
            Self::EmptyEnumVariants { span } => {
                write!(
                    f,
                    "enum at {span} has no variants; provide at least one quoted string"
                )
            }
            Self::DuplicateEnumVariant { variant, span } => {
                write!(f, "duplicate enum variant '{variant}' at {span}")
            }
            Self::InvalidIntegerRange { min, max, span } => {
                write!(
                    f,
                    "invalid integer range at {span}: min ({min}) > max ({max})"
                )
            }
        }
    }
}

impl std::error::Error for DslError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CoreSchemaError { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn span_display() {
        let span = Span::new(10, 20);
        assert_eq!(span.to_string(), "10..20");
    }

    #[test]
    fn error_display_invalid_token() {
        let err = DslError::InvalidToken {
            span: Span::new(0, 1),
        };
        let msg = err.to_string();
        assert!(msg.contains("invalid token"));
        assert!(msg.contains("0..1"));
    }

    #[test]
    fn error_display_unexpected_token() {
        let err = DslError::UnexpectedToken {
            expected: "'{'".into(),
            found: "'}'".into(),
            span: Span::new(5, 6),
        };
        let msg = err.to_string();
        assert!(msg.contains("expected '{'"));
        assert!(msg.contains("found '}'"));
    }

    #[test]
    fn error_display_unexpected_eof() {
        let err = DslError::UnexpectedEndOfInput {
            expected: "field definition".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("unexpected end of input"));
        assert!(msg.contains("field definition"));
    }

    #[test]
    fn error_display_invalid_schema_name() {
        let err = DslError::InvalidSchemaName {
            name: "contact".into(),
            span: Span::new(7, 14),
        };
        let msg = err.to_string();
        assert!(msg.contains("contact"));
        assert!(msg.contains("PascalCase"));
    }

    #[test]
    fn error_display_empty_schema() {
        let err = DslError::EmptySchema {
            name: "Empty".into(),
            span: Span::new(0, 15),
        };
        let msg = err.to_string();
        assert!(msg.contains("Empty"));
        assert!(msg.contains("no fields"));
    }

    #[test]
    fn error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(DslError::InvalidToken {
            span: Span::new(0, 1),
        });
        assert!(err.to_string().contains("invalid token"));
    }

    #[test]
    fn core_schema_error_has_source() {
        let err = DslError::CoreSchemaError {
            source: SchemaError::EmptyFields,
            span: Span::new(0, 10),
        };
        assert!(err.source().is_some());
    }

    #[test]
    fn non_core_errors_have_no_source() {
        let err = DslError::InvalidToken {
            span: Span::new(0, 1),
        };
        assert!(err.source().is_none());
    }
}
