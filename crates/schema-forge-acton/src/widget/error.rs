use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::ForgeError;

/// Widget-specific error wrapper that returns bare HTML error fragments.
pub struct WidgetError(pub ForgeError);

impl From<ForgeError> for WidgetError {
    fn from(err: ForgeError) -> Self {
        WidgetError(err)
    }
}

impl WidgetError {
    fn status_code(&self) -> StatusCode {
        match &self.0 {
            ForgeError::SchemaNotFound { .. } | ForgeError::EntityNotFound { .. } => {
                StatusCode::NOT_FOUND
            }
            ForgeError::Forbidden { .. } => StatusCode::FORBIDDEN,
            ForgeError::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            ForgeError::ValidationFailed { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            ForgeError::BackendUnavailable { .. } => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for WidgetError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let message = self.0.to_string();

        // Return a bare HTML error fragment suitable for HTMX swap
        let html = format!(
            r#"<div class="forge-error" data-status="{}">{}</div>"#,
            status.as_u16(),
            html_escape(&message),
        );

        (status, axum::response::Html(html)).into_response()
    }
}

impl WidgetError {
    pub fn schema_not_found(name: &str) -> Self {
        WidgetError(ForgeError::SchemaNotFound {
            name: name.to_string(),
        })
    }

    pub fn entity_not_found(schema: &str, id: &str) -> Self {
        WidgetError(ForgeError::EntityNotFound {
            schema: schema.to_string(),
            entity_id: id.to_string(),
        })
    }
}

/// Convert a `StatusCode` to a `WidgetError` for use in handler returns.
impl From<StatusCode> for WidgetError {
    fn from(status: StatusCode) -> Self {
        WidgetError(ForgeError::Internal {
            message: format!("HTTP {}", status),
        })
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
