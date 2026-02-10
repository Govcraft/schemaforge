use std::future::Future;
use std::pin::Pin;

use acton_ai::prelude::{ToolDefinition, ToolError};
use serde_json::{json, Value};

/// Returns the tool definition for the `read_schema_file` tool.
pub fn read_schema_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "read_schema_file".to_string(),
        description: "Read a .schema file from disk. Only absolute paths to .schema files \
                       are accepted for security."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to a .schema file"
                }
            },
            "required": ["path"]
        }),
    }
}

/// Returns an executor closure for the `read_schema_file` tool.
pub fn read_schema_file_executor(
) -> impl Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
       + Send
       + Sync
       + 'static {
    move |args: Value| {
        Box::pin(async move {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| {
                    ToolError::validation_failed(
                        "read_schema_file",
                        "missing required field 'path'",
                    )
                })?
                .to_string();

            // Security: only absolute paths
            if !path.starts_with('/') {
                return Ok(json!({
                    "status": "error",
                    "message": format!("Path must be absolute (start with '/'): '{path}'"),
                }));
            }

            // Security: only .schema files
            if !path.ends_with(".schema") {
                return Ok(json!({
                    "status": "error",
                    "message": format!("Only .schema files are allowed, got: '{path}'"),
                }));
            }

            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let size_bytes = content.len();
                    Ok(json!({
                        "status": "ok",
                        "path": path,
                        "content": content,
                        "size_bytes": size_bytes,
                    }))
                }
                Err(e) => Ok(json!({
                    "status": "error",
                    "message": format!("Failed to read '{path}': {e}"),
                })),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_has_correct_name_and_required_field() {
        let def = read_schema_file_tool_definition();
        assert_eq!(def.name, "read_schema_file");
        let required = def.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[tokio::test]
    async fn non_absolute_path_returns_error() {
        let executor = read_schema_file_executor();
        let result = executor(json!({"path": "relative/path.schema"}))
            .await
            .unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["message"].as_str().unwrap().contains("absolute"));
    }

    #[tokio::test]
    async fn non_schema_extension_returns_error() {
        let executor = read_schema_file_executor();
        let result = executor(json!({"path": "/tmp/test.txt"})).await.unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains(".schema files"));
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let executor = read_schema_file_executor();
        let result = executor(json!({"path": "/tmp/nonexistent_12345.schema"}))
            .await
            .unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["message"].as_str().unwrap().contains("Failed to read"));
    }
}
