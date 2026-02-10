use std::fmt;

/// Errors that occur during backend storage operations.
///
/// All variants carry enough context to produce actionable error messages.
/// Uses `String` for external error details to maintain `Clone` + `Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendError {
    /// Entity not found by ID within a given schema table.
    EntityNotFound {
        schema: String,
        entity_id: String,
    },
    /// Schema table not found by name.
    SchemaNotFound {
        schema: String,
    },
    /// Schema table already exists when attempting creation.
    SchemaAlreadyExists {
        schema: String,
    },
    /// Pre-write validation failed for a specific field.
    ValidationFailed {
        field: String,
        reason: String,
    },
    /// A required field was not provided.
    RequiredFieldMissing {
        field: String,
    },
    /// The provided value type does not match the field's expected type.
    TypeMismatch {
        field: String,
        expected: String,
        actual: String,
    },
    /// A migration step could not be applied.
    MigrationFailed {
        step: String,
        reason: String,
    },
    /// Connection or transport-level error.
    ConnectionError {
        message: String,
    },
    /// Query execution error.
    QueryError {
        message: String,
    },
    /// Internal or unexpected error.
    Internal {
        message: String,
    },
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntityNotFound { schema, entity_id } => {
                write!(
                    f,
                    "entity '{entity_id}' not found in schema '{schema}'"
                )
            }
            Self::SchemaNotFound { schema } => {
                write!(f, "schema '{schema}' not found")
            }
            Self::SchemaAlreadyExists { schema } => {
                write!(f, "schema '{schema}' already exists")
            }
            Self::ValidationFailed { field, reason } => {
                write!(
                    f,
                    "validation failed for field '{field}': {reason}"
                )
            }
            Self::RequiredFieldMissing { field } => {
                write!(f, "required field '{field}' is missing")
            }
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "type mismatch for field '{field}': expected {expected}, got {actual}"
                )
            }
            Self::MigrationFailed { step, reason } => {
                write!(f, "migration step failed ({step}): {reason}")
            }
            Self::ConnectionError { message } => {
                write!(f, "backend connection error: {message}")
            }
            Self::QueryError { message } => {
                write!(f, "query execution error: {message}")
            }
            Self::Internal { message } => {
                write!(f, "internal backend error: {message}")
            }
        }
    }
}

impl std::error::Error for BackendError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_not_found_display() {
        let err = BackendError::EntityNotFound {
            schema: "Contact".into(),
            entity_id: "entity_abc123".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("entity_abc123"));
        assert!(msg.contains("Contact"));
    }

    #[test]
    fn schema_not_found_display() {
        let err = BackendError::SchemaNotFound {
            schema: "Contact".into(),
        };
        assert_eq!(err.to_string(), "schema 'Contact' not found");
    }

    #[test]
    fn schema_already_exists_display() {
        let err = BackendError::SchemaAlreadyExists {
            schema: "Contact".into(),
        };
        assert_eq!(err.to_string(), "schema 'Contact' already exists");
    }

    #[test]
    fn validation_failed_display() {
        let err = BackendError::ValidationFailed {
            field: "email".into(),
            reason: "must contain '@'".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("email"));
        assert!(msg.contains("must contain '@'"));
    }

    #[test]
    fn required_field_missing_display() {
        let err = BackendError::RequiredFieldMissing {
            field: "name".into(),
        };
        assert_eq!(err.to_string(), "required field 'name' is missing");
    }

    #[test]
    fn type_mismatch_display() {
        let err = BackendError::TypeMismatch {
            field: "age".into(),
            expected: "Integer".into(),
            actual: "Text".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("age"));
        assert!(msg.contains("Integer"));
        assert!(msg.contains("Text"));
    }

    #[test]
    fn migration_failed_display() {
        let err = BackendError::MigrationFailed {
            step: "AddField { name: phone }".into(),
            reason: "table does not exist".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("AddField"));
        assert!(msg.contains("table does not exist"));
    }

    #[test]
    fn connection_error_display() {
        let err = BackendError::ConnectionError {
            message: "connection refused".into(),
        };
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn query_error_display() {
        let err = BackendError::QueryError {
            message: "syntax error near SELECT".into(),
        };
        assert!(err.to_string().contains("syntax error"));
    }

    #[test]
    fn internal_error_display() {
        let err = BackendError::Internal {
            message: "unexpected null".into(),
        };
        assert!(err.to_string().contains("unexpected null"));
    }

    #[test]
    fn error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(BackendError::SchemaNotFound {
            schema: "Test".into(),
        });
        assert!(err.to_string().contains("Test"));
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BackendError>();
    }
}
