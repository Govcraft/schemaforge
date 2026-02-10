use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::agent::SchemaForgeAgent;
use crate::error::ForgeAiError;

/// Request body for the `/forge/generate` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    /// Natural language description of the desired schema.
    pub description: String,
    /// If true, the agent will only validate and preview -- not apply.
    #[serde(default)]
    pub dry_run: bool,
    /// Optional named provider to use for generation.
    pub provider: Option<String>,
}

/// Response body for the `/forge/generate` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResponse {
    /// Status of the generation (e.g., "ok", "error").
    pub status: String,
    /// The generated response text from the LLM.
    pub response: String,
    /// The extracted DSL source (validated when possible).
    pub dsl: String,
    /// Where the DSL was extracted from (registry, tool_arguments, response_text, raw_text).
    pub source: String,
    /// Number of schemas found in the extracted DSL.
    pub schema_count: usize,
}

/// Axum handler for POST /forge/generate.
///
/// Sends the description to the SchemaForge agent and returns the response.
/// Uses `generate_dsl()` to reliably extract validated DSL from the LLM output.
pub async fn generate_handler(
    State(agent): State<Arc<SchemaForgeAgent>>,
    Json(request): Json<GenerateRequest>,
) -> Result<Json<GenerateResponse>, ForgeAiError> {
    let result = agent.generate_dsl(&request.description).await?;

    Ok(Json(GenerateResponse {
        status: "ok".to_string(),
        response: result.assistant_text,
        dsl: result.dsl,
        source: result.source.to_string(),
        schema_count: result.schema_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    #[test]
    fn generate_request_serialization() {
        let req = GenerateRequest {
            description: "Create a Contact schema".to_string(),
            dry_run: true,
            provider: Some("claude".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("Contact"));
        assert!(json.contains("claude"));
    }

    #[test]
    fn generate_request_deserialization() {
        let json = r#"{"description": "Create a Contact schema"}"#;
        let req: GenerateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.description, "Create a Contact schema");
        assert!(!req.dry_run); // default false
        assert!(req.provider.is_none());
    }

    #[test]
    fn generate_request_dry_run_defaults_to_false() {
        let json = r#"{"description": "test"}"#;
        let req: GenerateRequest = serde_json::from_str(json).unwrap();
        assert!(!req.dry_run);
    }

    #[test]
    fn generate_response_serialization() {
        let resp = GenerateResponse {
            status: "ok".to_string(),
            response: "Created schema Contact".to_string(),
            dsl: "schema Contact {\n    name: text required\n}".to_string(),
            source: "registry".to_string(),
            schema_count: 1,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("ok"));
        assert!(json.contains("Created schema Contact"));
        assert!(json.contains("registry"));
        assert!(json.contains("schema_count"));
    }

    #[test]
    fn generate_response_deserialization() {
        let json = r#"{"status": "ok", "response": "Done", "dsl": "schema X { name: text }", "source": "raw_text", "schema_count": 0}"#;
        let resp: GenerateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.response, "Done");
        assert_eq!(resp.source, "raw_text");
        assert_eq!(resp.schema_count, 0);
    }

    #[tokio::test]
    async fn forge_ai_error_into_response_status_codes() {
        let test_cases: Vec<(ForgeAiError, StatusCode)> = vec![
            (
                ForgeAiError::parse_failed(vec!["e".into()]),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (ForgeAiError::schema_not_found("X"), StatusCode::NOT_FOUND),
            (
                ForgeAiError::file_read_failed("p", "r"),
                StatusCode::BAD_REQUEST,
            ),
            (
                ForgeAiError::runtime_error("x"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (ForgeAiError::provider_error("x"), StatusCode::BAD_GATEWAY),
        ];

        for (err, expected_status) in test_cases {
            let response = err.into_response();
            assert_eq!(response.status(), expected_status);

            let body = response.into_body();
            let bytes = body.collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(json["error"].is_string());
            assert!(json["message"].is_string());
        }
    }
}
