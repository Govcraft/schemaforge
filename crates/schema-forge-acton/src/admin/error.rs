use std::fmt;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};

/// Errors returned by admin UI handlers.
#[derive(Debug)]
#[non_exhaustive]
pub enum AdminError {
    /// Unauthorized — redirect to login page.
    Unauthorized,
    /// Schema not found by name. Maps to 404.
    SchemaNotFound { name: String },
    /// Entity not found. Maps to 404.
    EntityNotFound { schema: String, entity_id: String },
    /// Form validation failed. Maps to 422.
    ValidationFailed { details: Vec<String> },
    /// Backend error. Maps to 502.
    BackendError { message: String },
    /// Internal error. Maps to 500.
    Internal { message: String },
}

impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "Unauthorized"),
            Self::SchemaNotFound { name } => write!(f, "Schema '{name}' not found"),
            Self::EntityNotFound { schema, entity_id } => {
                write!(f, "Entity '{entity_id}' not found in schema '{schema}'")
            }
            Self::ValidationFailed { details } => {
                write!(f, "Validation failed: {}", details.join("; "))
            }
            Self::BackendError { message } => write!(f, "Backend error: {message}"),
            Self::Internal { message } => write!(f, "Internal error: {message}"),
        }
    }
}

impl std::error::Error for AdminError {}

impl AdminError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::SchemaNotFound { .. } | Self::EntityNotFound { .. } => StatusCode::NOT_FOUND,
            Self::ValidationFailed { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BackendError { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        if matches!(self, Self::Unauthorized) {
            return Redirect::to("/admin/login").into_response();
        }
        let status = self.status_code();
        let message = self.to_string();
        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Error - SchemaForge Admin</title></head>
<body style="font-family: system-ui, sans-serif; max-width: 600px; margin: 50px auto; padding: 20px;">
<h1 style="color: #dc2626;">{} Error</h1>
<p>{}</p>
<a href="/admin/">&larr; Back to Dashboard</a>
</body>
</html>"#,
            status.as_u16(),
            html_escape(&message),
        );
        (status, Html(html)).into_response()
    }
}

impl From<schema_forge_backend::BackendError> for AdminError {
    fn from(err: schema_forge_backend::BackendError) -> Self {
        use schema_forge_backend::BackendError;
        match err {
            BackendError::EntityNotFound { schema, entity_id } => {
                Self::EntityNotFound { schema, entity_id }
            }
            BackendError::SchemaNotFound { schema } => Self::SchemaNotFound { name: schema },
            BackendError::ConnectionError { message } | BackendError::QueryError { message } => {
                Self::BackendError { message }
            }
            other => Self::Internal {
                message: other.to_string(),
            },
        }
    }
}

/// Basic HTML escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_unauthorized() {
        let err = AdminError::Unauthorized;
        assert_eq!(err.to_string(), "Unauthorized");
    }

    #[test]
    fn display_schema_not_found() {
        let err = AdminError::SchemaNotFound {
            name: "Contact".into(),
        };
        assert_eq!(err.to_string(), "Schema 'Contact' not found");
    }

    #[test]
    fn display_entity_not_found() {
        let err = AdminError::EntityNotFound {
            schema: "Contact".into(),
            entity_id: "entity_abc".into(),
        };
        assert!(err.to_string().contains("entity_abc"));
    }

    #[test]
    fn display_validation_failed() {
        let err = AdminError::ValidationFailed {
            details: vec!["field 'name': required".into()],
        };
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn status_codes() {
        assert_eq!(
            AdminError::Unauthorized.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            AdminError::SchemaNotFound { name: "X".into() }.status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            AdminError::EntityNotFound {
                schema: "X".into(),
                entity_id: "Y".into()
            }
            .status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            AdminError::ValidationFailed {
                details: vec!["x".into()]
            }
            .status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            AdminError::BackendError {
                message: "x".into()
            }
            .status_code(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            AdminError::Internal {
                message: "x".into()
            }
            .status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn html_escape_works() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a&b"), "a&amp;b");
    }

    #[test]
    fn admin_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AdminError>();
    }
}
