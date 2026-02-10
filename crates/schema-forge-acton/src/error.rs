use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schema_forge_backend::BackendError;

/// Errors returned by SchemaForge HTTP endpoints.
///
/// Each variant maps to a specific HTTP status code. All variants carry
/// enough context to produce actionable JSON error responses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForgeError {
    /// Schema not found by name. Maps to 404.
    SchemaNotFound { name: String },
    /// Entity not found by schema + id. Maps to 404.
    EntityNotFound { schema: String, entity_id: String },
    /// Schema already exists. Maps to 409 Conflict.
    SchemaAlreadyExists { name: String },
    /// Request body failed validation. Maps to 422.
    ValidationFailed { details: Vec<String> },
    /// Invalid schema name (not PascalCase). Maps to 400.
    InvalidSchemaName { name: String },
    /// Invalid entity ID format. Maps to 400.
    InvalidEntityId { id: String },
    /// Invalid query parameters. Maps to 400.
    InvalidQuery { message: String },
    /// Backend storage error. Maps to 502.
    BackendUnavailable { message: String },
    /// Internal error. Maps to 500.
    Internal { message: String },
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaNotFound { name } => {
                write!(f, "schema '{name}' not found")
            }
            Self::EntityNotFound { schema, entity_id } => {
                write!(f, "entity '{entity_id}' not found in schema '{schema}'")
            }
            Self::SchemaAlreadyExists { name } => {
                write!(f, "schema '{name}' already exists")
            }
            Self::ValidationFailed { details } => {
                write!(f, "validation failed: {}", details.join("; "))
            }
            Self::InvalidSchemaName { name } => {
                write!(
                    f,
                    "invalid schema name '{name}': must be PascalCase (e.g. 'Contact', 'MySchema')"
                )
            }
            Self::InvalidEntityId { id } => {
                write!(f, "invalid entity ID '{id}': must be a valid TypeID with 'entity' prefix")
            }
            Self::InvalidQuery { message } => {
                write!(f, "invalid query: {message}")
            }
            Self::BackendUnavailable { message } => {
                write!(f, "backend unavailable: {message}")
            }
            Self::Internal { message } => {
                write!(f, "internal error: {message}")
            }
        }
    }
}

impl std::error::Error for ForgeError {}

impl ForgeError {
    /// Returns the HTTP status code for this error variant.
    fn status_code(&self) -> StatusCode {
        match self {
            Self::SchemaNotFound { .. } | Self::EntityNotFound { .. } => StatusCode::NOT_FOUND,
            Self::SchemaAlreadyExists { .. } => StatusCode::CONFLICT,
            Self::ValidationFailed { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::InvalidSchemaName { .. }
            | Self::InvalidEntityId { .. }
            | Self::InvalidQuery { .. } => StatusCode::BAD_REQUEST,
            Self::BackendUnavailable { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Returns the error kind string used in JSON responses.
    fn error_kind(&self) -> &'static str {
        match self {
            Self::SchemaNotFound { .. } => "schema_not_found",
            Self::EntityNotFound { .. } => "entity_not_found",
            Self::SchemaAlreadyExists { .. } => "schema_already_exists",
            Self::ValidationFailed { .. } => "validation_failed",
            Self::InvalidSchemaName { .. } => "invalid_schema_name",
            Self::InvalidEntityId { .. } => "invalid_entity_id",
            Self::InvalidQuery { .. } => "invalid_query",
            Self::BackendUnavailable { .. } => "backend_unavailable",
            Self::Internal { .. } => "internal_error",
        }
    }
}

impl IntoResponse for ForgeError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = serde_json::json!({
            "error": self.error_kind(),
            "message": self.to_string(),
        });
        (status, axum::Json(body)).into_response()
    }
}

impl From<BackendError> for ForgeError {
    fn from(err: BackendError) -> Self {
        match err {
            BackendError::EntityNotFound { schema, entity_id } => {
                Self::EntityNotFound { schema, entity_id }
            }
            BackendError::SchemaNotFound { schema } => Self::SchemaNotFound { name: schema },
            BackendError::SchemaAlreadyExists { schema } => {
                Self::SchemaAlreadyExists { name: schema }
            }
            BackendError::ValidationFailed { field, reason } => Self::ValidationFailed {
                details: vec![format!("field '{field}': {reason}")],
            },
            BackendError::RequiredFieldMissing { field } => Self::ValidationFailed {
                details: vec![format!("required field '{field}' is missing")],
            },
            BackendError::TypeMismatch {
                field,
                expected,
                actual,
            } => Self::ValidationFailed {
                details: vec![format!(
                    "type mismatch for field '{field}': expected {expected}, got {actual}"
                )],
            },
            BackendError::MigrationFailed { step, reason } => Self::Internal {
                message: format!("migration step failed ({step}): {reason}"),
            },
            BackendError::ConnectionError { message } => {
                Self::BackendUnavailable { message }
            }
            BackendError::QueryError { message } => Self::BackendUnavailable { message },
            BackendError::Internal { message } => Self::Internal { message },
            _ => Self::Internal {
                message: err.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn display_schema_not_found() {
        let err = ForgeError::SchemaNotFound {
            name: "Contact".into(),
        };
        assert_eq!(err.to_string(), "schema 'Contact' not found");
    }

    #[test]
    fn display_entity_not_found() {
        let err = ForgeError::EntityNotFound {
            schema: "Contact".into(),
            entity_id: "entity_abc123".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("entity_abc123"));
        assert!(msg.contains("Contact"));
    }

    #[test]
    fn display_schema_already_exists() {
        let err = ForgeError::SchemaAlreadyExists {
            name: "Contact".into(),
        };
        assert_eq!(err.to_string(), "schema 'Contact' already exists");
    }

    #[test]
    fn display_validation_failed() {
        let err = ForgeError::ValidationFailed {
            details: vec!["field 'email': required".into(), "field 'name': too short".into()],
        };
        let msg = err.to_string();
        assert!(msg.contains("email"));
        assert!(msg.contains("name"));
    }

    #[test]
    fn display_invalid_schema_name() {
        let err = ForgeError::InvalidSchemaName {
            name: "bad_name".into(),
        };
        assert!(err.to_string().contains("bad_name"));
        assert!(err.to_string().contains("PascalCase"));
    }

    #[test]
    fn display_invalid_entity_id() {
        let err = ForgeError::InvalidEntityId {
            id: "not-valid".into(),
        };
        assert!(err.to_string().contains("not-valid"));
    }

    #[test]
    fn display_invalid_query() {
        let err = ForgeError::InvalidQuery {
            message: "missing filter value".into(),
        };
        assert!(err.to_string().contains("missing filter value"));
    }

    #[test]
    fn display_backend_unavailable() {
        let err = ForgeError::BackendUnavailable {
            message: "connection refused".into(),
        };
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn display_internal() {
        let err = ForgeError::Internal {
            message: "unexpected null".into(),
        };
        assert!(err.to_string().contains("unexpected null"));
    }

    #[test]
    fn status_codes() {
        assert_eq!(
            ForgeError::SchemaNotFound { name: "X".into() }.status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ForgeError::EntityNotFound {
                schema: "X".into(),
                entity_id: "Y".into()
            }
            .status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ForgeError::SchemaAlreadyExists { name: "X".into() }.status_code(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ForgeError::ValidationFailed {
                details: vec!["x".into()]
            }
            .status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            ForgeError::InvalidSchemaName { name: "X".into() }.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ForgeError::InvalidEntityId { id: "X".into() }.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ForgeError::InvalidQuery {
                message: "X".into()
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ForgeError::BackendUnavailable {
                message: "X".into()
            }
            .status_code(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            ForgeError::Internal {
                message: "X".into()
            }
            .status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn into_response_has_json_body() {
        let err = ForgeError::SchemaNotFound {
            name: "Contact".into(),
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = response.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "schema_not_found");
        assert!(json["message"].as_str().unwrap().contains("Contact"));
    }

    #[test]
    fn from_backend_entity_not_found() {
        let backend_err = BackendError::EntityNotFound {
            schema: "Contact".into(),
            entity_id: "entity_abc".into(),
        };
        let forge_err: ForgeError = backend_err.into();
        assert!(matches!(
            forge_err,
            ForgeError::EntityNotFound {
                schema,
                entity_id
            } if schema == "Contact" && entity_id == "entity_abc"
        ));
    }

    #[test]
    fn from_backend_schema_not_found() {
        let backend_err = BackendError::SchemaNotFound {
            schema: "Contact".into(),
        };
        let forge_err: ForgeError = backend_err.into();
        assert!(matches!(
            forge_err,
            ForgeError::SchemaNotFound { name } if name == "Contact"
        ));
    }

    #[test]
    fn from_backend_schema_already_exists() {
        let backend_err = BackendError::SchemaAlreadyExists {
            schema: "Contact".into(),
        };
        let forge_err: ForgeError = backend_err.into();
        assert!(matches!(
            forge_err,
            ForgeError::SchemaAlreadyExists { name } if name == "Contact"
        ));
    }

    #[test]
    fn from_backend_validation_failed() {
        let backend_err = BackendError::ValidationFailed {
            field: "email".into(),
            reason: "too long".into(),
        };
        let forge_err: ForgeError = backend_err.into();
        assert!(matches!(forge_err, ForgeError::ValidationFailed { details } if details.len() == 1));
    }

    #[test]
    fn from_backend_connection_error() {
        let backend_err = BackendError::ConnectionError {
            message: "refused".into(),
        };
        let forge_err: ForgeError = backend_err.into();
        assert!(matches!(
            forge_err,
            ForgeError::BackendUnavailable { message } if message == "refused"
        ));
    }

    #[test]
    fn from_backend_internal() {
        let backend_err = BackendError::Internal {
            message: "oops".into(),
        };
        let forge_err: ForgeError = backend_err.into();
        assert!(matches!(
            forge_err,
            ForgeError::Internal { message } if message == "oops"
        ));
    }

    #[test]
    fn forge_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ForgeError>();
    }

    #[test]
    fn forge_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(ForgeError::SchemaNotFound {
            name: "Test".into(),
        });
        assert!(err.to_string().contains("Test"));
    }
}
