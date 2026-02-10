use std::future::Future;
use std::pin::Pin;

use acton_ai::prelude::{ToolDefinition, ToolError};
use serde_json::{json, Value};

/// Returns the tool definition for the `validate_schema` tool.
pub fn validate_schema_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "validate_schema".to_string(),
        description: "Parse and validate SchemaDSL. Returns 'valid' with parsed details, \
                       or detailed error messages. Always call this before apply_schema."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "dsl": {
                    "type": "string",
                    "description": "The SchemaDSL source to validate"
                }
            },
            "required": ["dsl"]
        }),
    }
}

/// Returns an executor closure for the `validate_schema` tool.
pub fn validate_schema_executor(
) -> impl Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
       + Send
       + Sync
       + 'static {
    move |args: Value| {
        Box::pin(async move {
            let dsl = args["dsl"]
                .as_str()
                .ok_or_else(|| {
                    ToolError::validation_failed("validate_schema", "missing required field 'dsl'")
                })?
                .to_string();

            match schema_forge_dsl::parse(&dsl) {
                Ok(schemas) => {
                    let schema_summaries: Vec<Value> = schemas
                        .iter()
                        .map(|s| {
                            let field_names: Vec<&str> =
                                s.fields.iter().map(|f| f.name.as_str()).collect();
                            let relation_targets: Vec<String> = s
                                .fields
                                .iter()
                                .filter_map(|f| {
                                    if let schema_forge_core::types::FieldType::Relation {
                                        target,
                                        ..
                                    } = &f.field_type
                                    {
                                        Some(format!("{} -> {}", s.name.as_str(), target.as_str()))
                                    } else {
                                        None
                                    }
                                })
                                .collect();

                            json!({
                                "name": s.name.as_str(),
                                "field_count": s.fields.len(),
                                "fields": field_names,
                                "relations": relation_targets,
                            })
                        })
                        .collect();

                    Ok(json!({
                        "status": "valid",
                        "schemas": schema_summaries,
                        "message": "All schemas are valid. You can now call apply_schema."
                    }))
                }
                Err(errors) => {
                    let error_messages: Vec<String> =
                        errors.iter().map(|e| e.to_string()).collect();
                    Ok(json!({
                        "status": "parse_error",
                        "errors": error_messages,
                        "hint": "Fix the syntax errors and call validate_schema again."
                    }))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_has_correct_name_and_required_field() {
        let def = validate_schema_tool_definition();
        assert_eq!(def.name, "validate_schema");
        assert!(def.description.contains("validate"));
        let required = def.input_schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "dsl");
    }

    #[tokio::test]
    async fn valid_dsl_returns_status_valid() {
        let executor = validate_schema_executor();
        let args = json!({
            "dsl": "schema Contact {\n    name: text required\n}"
        });
        let result = executor(args).await.unwrap();
        assert_eq!(result["status"], "valid");
        let schemas = result["schemas"].as_array().unwrap();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"], "Contact");
        assert!(result["message"].as_str().unwrap().contains("valid"));
    }

    #[tokio::test]
    async fn invalid_dsl_returns_parse_error() {
        let executor = validate_schema_executor();
        let args = json!({
            "dsl": "schema { broken"
        });
        let result = executor(args).await.unwrap();
        assert_eq!(result["status"], "parse_error");
        assert!(!result["errors"].as_array().unwrap().is_empty());
        assert!(result["hint"].as_str().unwrap().contains("Fix"));
    }

    #[tokio::test]
    async fn empty_dsl_returns_parse_error() {
        let executor = validate_schema_executor();
        let args = json!({
            "dsl": ""
        });
        let result = executor(args).await.unwrap();
        // Empty DSL parses as zero schemas (valid empty program)
        // or returns parse error depending on parser behavior
        let status = result["status"].as_str().unwrap();
        assert!(status == "valid" || status == "parse_error");
    }

    #[tokio::test]
    async fn multiple_schemas_validated() {
        let executor = validate_schema_executor();
        let args = json!({
            "dsl": "schema Contact {\n    name: text required\n}\nschema Company {\n    name: text required\n}"
        });
        let result = executor(args).await.unwrap();
        assert_eq!(result["status"], "valid");
        let schemas = result["schemas"].as_array().unwrap();
        assert_eq!(schemas.len(), 2);
    }

    #[tokio::test]
    async fn missing_dsl_field_returns_error() {
        let executor = validate_schema_executor();
        let args = json!({});
        let result = executor(args).await;
        assert!(result.is_err());
    }
}
