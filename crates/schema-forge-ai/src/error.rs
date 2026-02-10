use std::fmt;

use acton_ai::prelude::{ActonAIError, ActonAIErrorKind};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Errors returned by the SchemaForge AI integration layer.
///
/// Each variant carries enough context to produce actionable error messages.
/// Maps to HTTP status codes via `IntoResponse` for use in axum handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForgeAiError {
    /// DSL source failed to parse.
    ParseFailed { errors: Vec<String> },
    /// Parsed schemas failed semantic validation.
    ValidationFailed { errors: Vec<String> },
    /// Schema application (diff + migrate + register) failed.
    ApplyFailed { reason: String },
    /// Named schema not found in the registry.
    SchemaNotFound { name: String },
    /// Could not read a .schema file from disk.
    FileReadFailed { path: String, reason: String },
    /// acton-ai runtime error (launch, shutdown, prompt failed).
    RuntimeError { reason: String },
    /// LLM provider error (rate limit, network, auth).
    ProviderError { reason: String },
    /// Configuration error (missing provider, bad config file).
    Configuration { field: String, reason: String },
    /// A tool closure returned an error.
    ToolExecutionFailed { tool: String, reason: String },
}

impl ForgeAiError {
    /// Creates a `ParseFailed` error from a list of parse error messages.
    #[must_use]
    pub fn parse_failed(errors: Vec<String>) -> Self {
        Self::ParseFailed { errors }
    }

    /// Creates a `ValidationFailed` error from a list of validation messages.
    #[must_use]
    pub fn validation_failed(errors: Vec<String>) -> Self {
        Self::ValidationFailed { errors }
    }

    /// Creates an `ApplyFailed` error.
    #[must_use]
    pub fn apply_failed(reason: impl Into<String>) -> Self {
        Self::ApplyFailed {
            reason: reason.into(),
        }
    }

    /// Creates a `SchemaNotFound` error.
    #[must_use]
    pub fn schema_not_found(name: impl Into<String>) -> Self {
        Self::SchemaNotFound { name: name.into() }
    }

    /// Creates a `FileReadFailed` error.
    #[must_use]
    pub fn file_read_failed(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::FileReadFailed {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// Creates a `RuntimeError`.
    #[must_use]
    pub fn runtime_error(reason: impl Into<String>) -> Self {
        Self::RuntimeError {
            reason: reason.into(),
        }
    }

    /// Creates a `ProviderError`.
    #[must_use]
    pub fn provider_error(reason: impl Into<String>) -> Self {
        Self::ProviderError {
            reason: reason.into(),
        }
    }

    /// Creates a `Configuration` error.
    #[must_use]
    pub fn configuration(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Configuration {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Creates a `ToolExecutionFailed` error.
    #[must_use]
    pub fn tool_execution_failed(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ToolExecutionFailed {
            tool: tool.into(),
            reason: reason.into(),
        }
    }

    /// Returns `true` if this is a `ParseFailed` variant.
    #[must_use]
    pub fn is_parse_failed(&self) -> bool {
        matches!(self, Self::ParseFailed { .. })
    }

    /// Returns `true` if this is a `SchemaNotFound` variant.
    #[must_use]
    pub fn is_schema_not_found(&self) -> bool {
        matches!(self, Self::SchemaNotFound { .. })
    }

    /// Returns `true` if this is a `Configuration` variant.
    #[must_use]
    pub fn is_configuration(&self) -> bool {
        matches!(self, Self::Configuration { .. })
    }

    /// Returns the HTTP status code for this error variant.
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ParseFailed { .. } | Self::ValidationFailed { .. } => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::SchemaNotFound { .. } => StatusCode::NOT_FOUND,
            Self::FileReadFailed { .. } | Self::Configuration { .. } => StatusCode::BAD_REQUEST,
            Self::RuntimeError { .. }
            | Self::ApplyFailed { .. }
            | Self::ToolExecutionFailed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ProviderError { .. } => StatusCode::BAD_GATEWAY,
        }
    }

    /// Returns the error kind string used in JSON error responses.
    fn error_kind(&self) -> &'static str {
        match self {
            Self::ParseFailed { .. } => "parse_failed",
            Self::ValidationFailed { .. } => "validation_failed",
            Self::ApplyFailed { .. } => "apply_failed",
            Self::SchemaNotFound { .. } => "schema_not_found",
            Self::FileReadFailed { .. } => "file_read_failed",
            Self::RuntimeError { .. } => "runtime_error",
            Self::ProviderError { .. } => "provider_error",
            Self::Configuration { .. } => "configuration",
            Self::ToolExecutionFailed { .. } => "tool_execution_failed",
        }
    }
}

impl fmt::Display for ForgeAiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParseFailed { errors } => {
                write!(f, "DSL parse failed: {}", errors.join("; "))
            }
            Self::ValidationFailed { errors } => {
                write!(f, "schema validation failed: {}", errors.join("; "))
            }
            Self::ApplyFailed { reason } => {
                write!(f, "failed to apply schema: {reason}")
            }
            Self::SchemaNotFound { name } => {
                write!(f, "schema '{name}' not found in registry")
            }
            Self::FileReadFailed { path, reason } => {
                write!(f, "failed to read schema file '{path}': {reason}")
            }
            Self::RuntimeError { reason } => {
                write!(f, "AI runtime error: {reason}")
            }
            Self::ProviderError { reason } => {
                write!(f, "LLM provider error: {reason}")
            }
            Self::Configuration { field, reason } => {
                write!(f, "configuration error for '{field}': {reason}")
            }
            Self::ToolExecutionFailed { tool, reason } => {
                write!(f, "tool '{tool}' failed: {reason}")
            }
        }
    }
}

impl std::error::Error for ForgeAiError {}

impl From<ActonAIError> for ForgeAiError {
    fn from(err: ActonAIError) -> Self {
        match err.kind {
            ActonAIErrorKind::Configuration { field, reason } => {
                Self::Configuration { field, reason }
            }
            ActonAIErrorKind::ProviderError { reason } => Self::ProviderError { reason },
            ActonAIErrorKind::RuntimeShutdown => {
                Self::RuntimeError {
                    reason: "runtime has been shut down".to_string(),
                }
            }
            ActonAIErrorKind::LaunchFailed { reason }
            | ActonAIErrorKind::PromptFailed { reason }
            | ActonAIErrorKind::StreamError { reason } => Self::RuntimeError { reason },
        }
    }
}

impl IntoResponse for ForgeAiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = serde_json::json!({
            "error": self.error_kind(),
            "message": self.to_string(),
        });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    // -----------------------------------------------------------------------
    // Display tests for each variant
    // -----------------------------------------------------------------------

    #[test]
    fn display_parse_failed() {
        let err = ForgeAiError::parse_failed(vec![
            "unexpected token at line 3".into(),
            "missing closing brace".into(),
        ]);
        let msg = err.to_string();
        assert_eq!(
            msg,
            "DSL parse failed: unexpected token at line 3; missing closing brace"
        );
    }

    #[test]
    fn display_validation_failed() {
        let err = ForgeAiError::validation_failed(vec!["field 'name' is required".into()]);
        assert_eq!(
            err.to_string(),
            "schema validation failed: field 'name' is required"
        );
    }

    #[test]
    fn display_apply_failed() {
        let err = ForgeAiError::apply_failed("migration step 3 failed");
        assert_eq!(
            err.to_string(),
            "failed to apply schema: migration step 3 failed"
        );
    }

    #[test]
    fn display_schema_not_found() {
        let err = ForgeAiError::schema_not_found("Contact");
        assert_eq!(err.to_string(), "schema 'Contact' not found in registry");
    }

    #[test]
    fn display_file_read_failed() {
        let err = ForgeAiError::file_read_failed("/tmp/test.schema", "permission denied");
        assert_eq!(
            err.to_string(),
            "failed to read schema file '/tmp/test.schema': permission denied"
        );
    }

    #[test]
    fn display_runtime_error() {
        let err = ForgeAiError::runtime_error("actor system crashed");
        assert_eq!(err.to_string(), "AI runtime error: actor system crashed");
    }

    #[test]
    fn display_provider_error() {
        let err = ForgeAiError::provider_error("rate limit exceeded");
        assert_eq!(err.to_string(), "LLM provider error: rate limit exceeded");
    }

    #[test]
    fn display_configuration() {
        let err = ForgeAiError::configuration("api_key", "cannot be empty");
        assert_eq!(
            err.to_string(),
            "configuration error for 'api_key': cannot be empty"
        );
    }

    #[test]
    fn display_tool_execution_failed() {
        let err = ForgeAiError::tool_execution_failed("validate_schema", "invalid JSON input");
        assert_eq!(
            err.to_string(),
            "tool 'validate_schema' failed: invalid JSON input"
        );
    }

    // -----------------------------------------------------------------------
    // Predicate tests
    // -----------------------------------------------------------------------

    #[test]
    fn is_parse_failed_positive() {
        let err = ForgeAiError::parse_failed(vec![]);
        assert!(err.is_parse_failed());
    }

    #[test]
    fn is_parse_failed_negative() {
        let err = ForgeAiError::runtime_error("test");
        assert!(!err.is_parse_failed());
    }

    #[test]
    fn is_schema_not_found_positive() {
        let err = ForgeAiError::schema_not_found("X");
        assert!(err.is_schema_not_found());
    }

    #[test]
    fn is_schema_not_found_negative() {
        let err = ForgeAiError::parse_failed(vec![]);
        assert!(!err.is_schema_not_found());
    }

    #[test]
    fn is_configuration_positive() {
        let err = ForgeAiError::configuration("f", "r");
        assert!(err.is_configuration());
    }

    #[test]
    fn is_configuration_negative() {
        let err = ForgeAiError::provider_error("x");
        assert!(!err.is_configuration());
    }

    // -----------------------------------------------------------------------
    // Clone + Eq
    // -----------------------------------------------------------------------

    #[test]
    fn clone_and_eq() {
        let err = ForgeAiError::schema_not_found("Test");
        let cloned = err.clone();
        assert_eq!(err, cloned);
    }

    // -----------------------------------------------------------------------
    // std::error::Error trait object compatibility
    // -----------------------------------------------------------------------

    #[test]
    fn is_std_error() {
        let err: Box<dyn std::error::Error> =
            Box::new(ForgeAiError::schema_not_found("Contact"));
        assert!(err.to_string().contains("Contact"));
    }

    // -----------------------------------------------------------------------
    // From<ActonAIError> conversions
    // -----------------------------------------------------------------------

    #[test]
    fn from_acton_ai_configuration() {
        let acton_err = ActonAIError::configuration("provider", "not set");
        let forge_err: ForgeAiError = acton_err.into();
        assert!(forge_err.is_configuration());
        assert!(forge_err.to_string().contains("provider"));
    }

    #[test]
    fn from_acton_ai_prompt_failed() {
        let acton_err = ActonAIError::prompt_failed("timeout");
        let forge_err: ForgeAiError = acton_err.into();
        assert!(matches!(forge_err, ForgeAiError::RuntimeError { .. }));
        assert!(forge_err.to_string().contains("timeout"));
    }

    #[test]
    fn from_acton_ai_provider_error() {
        let acton_err = ActonAIError::provider_error("rate limit");
        let forge_err: ForgeAiError = acton_err.into();
        assert!(matches!(forge_err, ForgeAiError::ProviderError { .. }));
    }

    #[test]
    fn from_acton_ai_runtime_shutdown() {
        let acton_err = ActonAIError::runtime_shutdown();
        let forge_err: ForgeAiError = acton_err.into();
        assert!(matches!(forge_err, ForgeAiError::RuntimeError { .. }));
        assert!(forge_err.to_string().contains("shut down"));
    }

    // -----------------------------------------------------------------------
    // Status codes
    // -----------------------------------------------------------------------

    #[test]
    fn status_codes() {
        assert_eq!(
            ForgeAiError::parse_failed(vec![]).status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            ForgeAiError::validation_failed(vec![]).status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            ForgeAiError::schema_not_found("X").status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ForgeAiError::file_read_failed("p", "r").status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ForgeAiError::configuration("f", "r").status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ForgeAiError::runtime_error("x").status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ForgeAiError::apply_failed("x").status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ForgeAiError::tool_execution_failed("t", "r").status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ForgeAiError::provider_error("x").status_code(),
            StatusCode::BAD_GATEWAY
        );
    }

    // -----------------------------------------------------------------------
    // IntoResponse
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn into_response_has_json_body() {
        let err = ForgeAiError::schema_not_found("Contact");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = response.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "schema_not_found");
        assert!(json["message"].as_str().unwrap().contains("Contact"));
    }

    // -----------------------------------------------------------------------
    // Send + Sync
    // -----------------------------------------------------------------------

    #[test]
    fn forge_ai_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ForgeAiError>();
    }
}
