use std::fmt;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};

/// Errors returned by site UI handlers.
#[derive(Debug)]
#[non_exhaustive]
pub enum SiteError {
    /// Unauthorized — redirect to login page.
    Unauthorized,
    /// Internal error. Maps to 500.
    Internal { message: String },
}

impl fmt::Display for SiteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "Unauthorized"),
            Self::Internal { message } => write!(f, "Internal error: {message}"),
        }
    }
}

impl std::error::Error for SiteError {}

impl SiteError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for SiteError {
    fn into_response(self) -> Response {
        if matches!(self, Self::Unauthorized) {
            return Redirect::to("/site/login").into_response();
        }
        let status = self.status_code();
        let message = self.to_string();
        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Error - SchemaForge</title></head>
<body style="font-family: system-ui, sans-serif; max-width: 600px; margin: 50px auto; padding: 20px;">
<h1 style="color: #dc2626;">{} Error</h1>
<p>{}</p>
<a href="/site/">&larr; Back to Home</a>
</body>
</html>"#,
            status.as_u16(),
            html_escape(&message),
        );
        (status, Html(html)).into_response()
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
        let err = SiteError::Unauthorized;
        assert_eq!(err.to_string(), "Unauthorized");
    }

    #[test]
    fn display_internal() {
        let err = SiteError::Internal {
            message: "db down".into(),
        };
        assert!(err.to_string().contains("db down"));
    }

    #[test]
    fn status_codes() {
        assert_eq!(
            SiteError::Unauthorized.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            SiteError::Internal {
                message: "x".into()
            }
            .status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn html_escape_works() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
    }

    #[test]
    fn site_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SiteError>();
    }
}
