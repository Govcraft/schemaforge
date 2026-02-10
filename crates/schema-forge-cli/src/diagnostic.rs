use miette::{Diagnostic, NamedSource, SourceSpan};
use schema_forge_dsl::DslError;

/// A diagnostic wrapping a `DslError` for rich miette rendering.
///
/// Provides source code highlighting, span labels, and actionable suggestions
/// when rendering parse errors in human-readable mode.
///
/// The module-level `#[allow(unused_assignments)]` in main.rs is required
/// because miette's derive macro generates assignment patterns that rustc
/// flags as unused.
#[derive(Debug, thiserror::Error, Diagnostic)]
#[error("{message}")]
pub struct SchemaDiagnostic {
    #[source_code]
    src: NamedSource<String>,

    #[label("{label}")]
    span: SourceSpan,

    message: String,
    label: String,

    #[help]
    suggestion: Option<String>,
}

/// Convert a `DslError` into a miette `SchemaDiagnostic`.
///
/// Extracts span information from the error variant and produces
/// appropriate labels and suggestions for each error type.
pub fn dsl_error_to_diagnostic(error: &DslError, source: &str, filename: &str) -> SchemaDiagnostic {
    let named_src = NamedSource::new(filename, source.to_string());

    match error {
        DslError::InvalidToken { span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: "invalid token".to_string(),
            label: "unrecognized token".to_string(),
            suggestion: Some("Check for typos or unsupported characters.".to_string()),
        },

        DslError::UnexpectedToken {
            expected,
            found,
            span,
        } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("unexpected token: expected {expected}, found {found}"),
            label: format!("expected {expected}"),
            suggestion: None,
        },

        DslError::UnexpectedEndOfInput { expected } => SchemaDiagnostic {
            src: named_src,
            span: (source.len().saturating_sub(1), 1).into(),
            message: format!("unexpected end of input: expected {expected}"),
            label: "input ended here".to_string(),
            suggestion: Some(format!("Add {expected} to complete the definition.")),
        },

        DslError::InvalidSchemaName { name, span } => {
            let suggestion = to_pascal_case(name);
            SchemaDiagnostic {
                src: named_src,
                span: (span.start, span.end.saturating_sub(span.start)).into(),
                message: format!("invalid schema name '{name}'"),
                label: "must be PascalCase [A-Z][a-zA-Z0-9]*".to_string(),
                suggestion: Some(format!("Rename to '{suggestion}'.")),
            }
        }

        DslError::InvalidFieldName { name, span } => {
            let suggestion = to_snake_case(name);
            SchemaDiagnostic {
                src: named_src,
                span: (span.start, span.end.saturating_sub(span.start)).into(),
                message: format!("invalid field name '{name}'"),
                label: "must be snake_case [a-z][a-z0-9_]*".to_string(),
                suggestion: Some(format!("Rename to '{suggestion}'.")),
            }
        }

        DslError::DuplicateFieldName { name, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("duplicate field name '{name}'"),
            label: "already defined above".to_string(),
            suggestion: Some("Remove the duplicate or rename one of the fields.".to_string()),
        },

        DslError::DuplicateAnnotation { kind, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("duplicate annotation '@{kind}'"),
            label: "already applied above".to_string(),
            suggestion: Some("Remove the duplicate annotation.".to_string()),
        },

        DslError::EmptySchema { name, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("schema '{name}' has no fields"),
            label: "empty schema body".to_string(),
            suggestion: Some("Add at least one field definition inside the braces.".to_string()),
        },

        DslError::InvalidIntegerLiteral { text, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("invalid integer literal '{text}'"),
            label: "expected a valid integer".to_string(),
            suggestion: None,
        },

        DslError::InvalidFloatLiteral { text, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("invalid float literal '{text}'"),
            label: "expected a valid number".to_string(),
            suggestion: None,
        },

        DslError::CoreSchemaError { source, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("schema validation error: {source}"),
            label: "validation failed".to_string(),
            suggestion: None,
        },

        DslError::EmptyEnumVariants { span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: "enum has no variants".to_string(),
            label: "empty variant list".to_string(),
            suggestion: Some(
                "Provide at least one quoted string, e.g. enum(\"Active\", \"Inactive\")"
                    .to_string(),
            ),
        },

        DslError::DuplicateEnumVariant { variant, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("duplicate enum variant '{variant}'"),
            label: "already listed above".to_string(),
            suggestion: Some("Remove the duplicate variant.".to_string()),
        },

        DslError::InvalidIntegerRange { min, max, span } => SchemaDiagnostic {
            src: named_src,
            span: (span.start, span.end.saturating_sub(span.start)).into(),
            message: format!("invalid integer range: min ({min}) > max ({max})"),
            label: "min must be <= max".to_string(),
            suggestion: Some(format!("Swap the values: integer(min: {max}, max: {min})")),
        },

        // Catch future non_exhaustive variants
        _ => SchemaDiagnostic {
            src: named_src,
            span: (0, 0).into(),
            message: error.to_string(),
            label: "error".to_string(),
            suggestion: None,
        },
    }
}

/// Render all parse errors for a file using miette.
///
/// Returns a vector of `miette::Report` that can be printed to stderr.
pub fn render_diagnostics(
    errors: &[DslError],
    source: &str,
    filename: &str,
) -> Vec<miette::Report> {
    errors
        .iter()
        .map(|e| {
            let diagnostic = dsl_error_to_diagnostic(e, source, filename);
            miette::Report::new(diagnostic)
        })
        .collect()
}

/// Simple PascalCase converter: capitalizes first letter of each word boundary.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Simple snake_case converter: inserts underscores before uppercase letters.
fn to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_dsl::Span;

    #[test]
    fn invalid_token_diagnostic() {
        let err = DslError::InvalidToken {
            span: Span::new(0, 3),
        };
        let diag = dsl_error_to_diagnostic(&err, "???", "test.schema");
        assert!(diag.message.contains("invalid token"));
        assert!(diag.suggestion.is_some());
    }

    #[test]
    fn unexpected_token_diagnostic() {
        let err = DslError::UnexpectedToken {
            expected: "'{'".into(),
            found: "'}'".into(),
            span: Span::new(5, 6),
        };
        let diag = dsl_error_to_diagnostic(&err, "schema X }", "test.schema");
        assert!(diag.message.contains("expected '{'"));
        assert!(diag.label.contains("expected '{'"));
    }

    #[test]
    fn unexpected_end_of_input_diagnostic() {
        let err = DslError::UnexpectedEndOfInput {
            expected: "field definition".into(),
        };
        let diag = dsl_error_to_diagnostic(&err, "schema X {", "test.schema");
        assert!(diag.message.contains("unexpected end"));
        assert!(diag.suggestion.is_some());
    }

    #[test]
    fn invalid_schema_name_suggests_pascal_case() {
        let err = DslError::InvalidSchemaName {
            name: "contact".into(),
            span: Span::new(7, 14),
        };
        let diag = dsl_error_to_diagnostic(&err, "schema contact {}", "test.schema");
        assert!(diag.suggestion.as_ref().unwrap().contains("Contact"));
    }

    #[test]
    fn invalid_field_name_suggests_snake_case() {
        let err = DslError::InvalidFieldName {
            name: "firstName".into(),
            span: Span::new(20, 29),
        };
        let diag = dsl_error_to_diagnostic(&err, "schema X { firstName: text }", "test.schema");
        assert!(diag.suggestion.as_ref().unwrap().contains("first_name"));
    }

    #[test]
    fn duplicate_field_name_diagnostic() {
        let err = DslError::DuplicateFieldName {
            name: "email".into(),
            span: Span::new(30, 35),
        };
        let diag =
            dsl_error_to_diagnostic(&err, "schema X { email: text\nemail: text }", "t.schema");
        assert!(diag.message.contains("duplicate field"));
        assert!(diag.label.contains("already defined"));
    }

    #[test]
    fn empty_schema_diagnostic() {
        let err = DslError::EmptySchema {
            name: "Empty".into(),
            span: Span::new(0, 15),
        };
        let diag = dsl_error_to_diagnostic(&err, "schema Empty {}", "test.schema");
        assert!(diag.message.contains("no fields"));
        assert!(diag
            .suggestion
            .as_ref()
            .unwrap()
            .contains("Add at least one"));
    }

    #[test]
    fn empty_enum_variants_diagnostic() {
        let err = DslError::EmptyEnumVariants {
            span: Span::new(10, 15),
        };
        let diag = dsl_error_to_diagnostic(&err, "status: enum()", "test.schema");
        assert!(diag.message.contains("no variants"));
    }

    #[test]
    fn invalid_integer_range_diagnostic() {
        let err = DslError::InvalidIntegerRange {
            min: 100,
            max: 10,
            span: Span::new(5, 25),
        };
        let diag = dsl_error_to_diagnostic(&err, "age: integer(min: 100, max: 10)", "test.schema");
        assert!(diag.message.contains("100"));
        assert!(diag.message.contains("10"));
        assert!(diag.suggestion.as_ref().unwrap().contains("Swap"));
    }

    #[test]
    fn render_diagnostics_produces_reports() {
        let errors = vec![
            DslError::InvalidToken {
                span: Span::new(0, 1),
            },
            DslError::EmptySchema {
                name: "X".into(),
                span: Span::new(0, 5),
            },
        ];
        let reports = render_diagnostics(&errors, "??? X", "test.schema");
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn to_pascal_case_converts() {
        assert_eq!(to_pascal_case("contact"), "Contact");
        assert_eq!(to_pascal_case("my_schema"), "MySchema");
        assert_eq!(to_pascal_case("already_good"), "AlreadyGood");
    }

    #[test]
    fn to_snake_case_converts() {
        assert_eq!(to_snake_case("firstName"), "first_name");
        assert_eq!(to_snake_case("MyField"), "my_field");
        assert_eq!(to_snake_case("already_ok"), "already_ok");
    }
}
