use std::future::Future;
use std::pin::Pin;

use acton_ai::prelude::{ToolDefinition, ToolError};
use schema_forge_acton::SchemaRegistry;
use schema_forge_core::types::{Annotation, SchemaDefinition};
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

            // Build access summary from @access annotations
            let access_summary: serde_json::Map<String, Value> = schemas
                .iter()
                .map(|s| {
                    let summary = build_access_summary(s);
                    (s.name.as_str().to_string(), summary)
                })
                .collect();

            Ok(json!({
                "schema_count": schemas.len(),
                "schemas_dsl": dsl_text,
                "schema_names": names,
                "access_summary": access_summary,
            }))
        })
    }
}

/// Build an access summary for a schema from its `@access` annotation.
///
/// Returns a JSON object with `has_access: true` and role lists if `@access`
/// is present, or `has_access: false` if no access annotation is defined.
fn build_access_summary(schema: &SchemaDefinition) -> Value {
    for annotation in &schema.annotations {
        if let Annotation::Access {
            read,
            write,
            delete,
            ..
        } = annotation
        {
            return json!({
                "has_access": true,
                "read_roles": read,
                "write_roles": write,
                "delete_roles": delete,
            });
        }
    }
    json!({ "has_access": false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Annotation, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
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

    fn make_schema_with_access(name: &str) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![Annotation::Access {
                read: vec!["viewer".to_string()],
                write: vec!["editor".to_string()],
                delete: vec!["admin".to_string()],
                cross_tenant_read: vec![],
            }],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn list_schemas_includes_access_summary() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Article".to_string(), make_schema_with_access("Article"))
            .await;
        registry
            .insert("Note".to_string(), make_test_schema("Note"))
            .await;

        let executor = list_schemas_executor(registry);
        let result = executor(json!({})).await.unwrap();

        let summary = &result["access_summary"];
        assert!(summary.is_object());

        // Article has @access
        let article = &summary["Article"];
        assert_eq!(article["has_access"], true);
        assert!(article["read_roles"]
            .as_array()
            .unwrap()
            .contains(&json!("viewer")));
        assert!(article["write_roles"]
            .as_array()
            .unwrap()
            .contains(&json!("editor")));

        // Note has no @access
        let note = &summary["Note"];
        assert_eq!(note["has_access"], false);
    }
}
