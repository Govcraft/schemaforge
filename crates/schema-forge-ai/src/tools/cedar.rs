use std::future::Future;
use std::pin::Pin;

use acton_ai::prelude::{ToolDefinition, ToolError};
use schema_forge_acton::SchemaRegistry;
use serde_json::{json, Value};

/// Returns the tool definition for the `generate_cedar` tool.
pub fn generate_cedar_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "generate_cedar".to_string(),
        description: "Generate Cedar authorization policies for a registered schema. \
                       The schema must already be applied via apply_schema."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "schema_name": {
                    "type": "string",
                    "description": "The PascalCase name of the schema to generate policies for"
                }
            },
            "required": ["schema_name"]
        }),
    }
}

/// Returns an executor closure for the `generate_cedar` tool.
pub fn generate_cedar_executor(
    registry: SchemaRegistry,
) -> impl Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
       + Send
       + Sync
       + 'static {
    move |args: Value| {
        let registry = registry.clone();
        Box::pin(async move {
            let schema_name = args["schema_name"]
                .as_str()
                .ok_or_else(|| {
                    ToolError::validation_failed(
                        "generate_cedar",
                        "missing required field 'schema_name'",
                    )
                })?
                .to_string();

            let schema = registry.get(&schema_name).await;
            let Some(schema) = schema else {
                return Ok(json!({
                    "status": "error",
                    "message": format!("Schema '{schema_name}' not found"),
                }));
            };

            let policies = schema_forge_acton::cedar::generate_cedar_policies(&schema);
            let policy_text: String = policies
                .iter()
                .map(|p| format!("// {}\n{}", p.description, p.cedar_text))
                .collect::<Vec<_>>()
                .join("\n\n");

            Ok(json!({
                "status": "generated",
                "schema": schema_name,
                "policy_count": policies.len(),
                "policies": policy_text,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
        TextConstraints,
    };

    fn make_test_schema(name: &str) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn definition_has_correct_name() {
        let def = generate_cedar_tool_definition();
        assert_eq!(def.name, "generate_cedar");
    }

    #[tokio::test]
    async fn missing_schema_returns_error() {
        let registry = SchemaRegistry::new();
        let executor = generate_cedar_executor(registry);
        let result = executor(json!({"schema_name": "Missing"})).await.unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["message"].as_str().unwrap().contains("Missing"));
    }

    #[tokio::test]
    async fn valid_schema_returns_policies() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Contact".to_string(), make_test_schema("Contact"))
            .await;

        let executor = generate_cedar_executor(registry);
        let result = executor(json!({"schema_name": "Contact"})).await.unwrap();
        assert_eq!(result["status"], "generated");
        assert_eq!(result["policy_count"], 4);
    }

    #[tokio::test]
    async fn policy_text_contains_schema_name() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Contact".to_string(), make_test_schema("Contact"))
            .await;

        let executor = generate_cedar_executor(registry);
        let result = executor(json!({"schema_name": "Contact"})).await.unwrap();
        let policies = result["policies"].as_str().unwrap();
        assert!(policies.contains("Contact"));
    }
}
