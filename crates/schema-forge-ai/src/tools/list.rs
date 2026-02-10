use std::future::Future;
use std::pin::Pin;

use acton_ai::prelude::{ToolDefinition, ToolError};
use schema_forge_acton::SchemaRegistry;
use serde_json::{json, Value};

/// Returns the tool definition for the `list_schemas` tool.
pub fn list_schemas_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_schemas".to_string(),
        description: "List all registered schemas. Returns schema count, names, and \
                       DSL text of all schemas in the registry."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

/// Returns an executor closure for the `list_schemas` tool.
pub fn list_schemas_executor(
    registry: SchemaRegistry,
) -> impl Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
       + Send
       + Sync
       + 'static {
    move |_args: Value| {
        let registry = registry.clone();
        Box::pin(async move {
            let schemas = registry.list().await;

            if schemas.is_empty() {
                return Ok(json!({
                    "schema_count": 0,
                    "schemas_dsl": "",
                    "schema_names": []
                }));
            }

            let dsl_text = schema_forge_dsl::print_all(&schemas);
            let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();

            Ok(json!({
                "schema_count": schemas.len(),
                "schemas_dsl": dsl_text,
                "schema_names": names,
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
        let def = list_schemas_tool_definition();
        assert_eq!(def.name, "list_schemas");
    }

    #[tokio::test]
    async fn empty_registry_returns_count_zero() {
        let registry = SchemaRegistry::new();
        let executor = list_schemas_executor(registry);
        let result = executor(json!({})).await.unwrap();
        assert_eq!(result["schema_count"], 0);
        assert_eq!(result["schemas_dsl"], "");
        assert!(result["schema_names"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn registry_with_schemas_returns_dsl_text() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Contact".to_string(), make_test_schema("Contact"))
            .await;
        registry
            .insert("Company".to_string(), make_test_schema("Company"))
            .await;

        let executor = list_schemas_executor(registry);
        let result = executor(json!({})).await.unwrap();
        assert_eq!(result["schema_count"], 2);
        let dsl = result["schemas_dsl"].as_str().unwrap();
        assert!(!dsl.is_empty());
        let names = result["schema_names"].as_array().unwrap();
        assert_eq!(names.len(), 2);
    }
}
