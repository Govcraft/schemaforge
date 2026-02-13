use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use acton_ai::prelude::{ToolDefinition, ToolError};
use schema_forge_acton::state::{DynForgeBackend, SchemaRegistry};
use schema_forge_core::migration::DiffEngine;
use serde_json::{json, Value};

/// Returns the tool definition for the `apply_schema` tool.
pub fn apply_schema_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "apply_schema".to_string(),
        description: "Parse, diff, and apply SchemaDSL to the backend. Supports dry_run mode \
                       to preview migration steps without applying them."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "dsl": {
                    "type": "string",
                    "description": "The SchemaDSL source to apply"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, preview migration steps without applying. Defaults to false.",
                    "default": false
                }
            },
            "required": ["dsl"]
        }),
    }
}

/// Returns an executor closure for the `apply_schema` tool.
pub fn apply_schema_executor(
    registry: SchemaRegistry,
    backend: Arc<dyn DynForgeBackend>,
) -> impl Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
       + Send
       + Sync
       + 'static {
    move |args: Value| {
        let registry = registry.clone();
        let backend = backend.clone();
        Box::pin(async move {
            let dsl = args["dsl"]
                .as_str()
                .ok_or_else(|| {
                    ToolError::validation_failed("apply_schema", "missing required field 'dsl'")
                })?
                .to_string();

            let dry_run = args["dry_run"].as_bool().unwrap_or(false);

            let schemas = match schema_forge_dsl::parse(&dsl) {
                Ok(s) => s,
                Err(errors) => {
                    let error_messages: Vec<String> =
                        errors.iter().map(|e| e.to_string()).collect();
                    return Ok(json!({
                        "status": "parse_error",
                        "errors": error_messages,
                    }));
                }
            };

            let mut results = Vec::new();

            for schema in &schemas {
                let name = schema.name.as_str().to_string();
                let existing = registry.get(&name).await;

                let plan = if let Some(ref old) = existing {
                    DiffEngine::diff(old, schema)
                } else {
                    DiffEngine::create_new(schema)
                };

                let step_descriptions: Vec<String> =
                    plan.steps.iter().map(|s| s.to_string()).collect();

                if dry_run {
                    results.push(json!({
                        "schema": name,
                        "action": if existing.is_some() { "update" } else { "create" },
                        "migration_steps": step_descriptions,
                    }));
                } else {
                    // Apply migration
                    if let Err(e) = backend.apply_migration(&schema.name, &plan.steps).await {
                        return Ok(json!({
                            "status": "error",
                            "schema": name,
                            "message": format!("migration failed: {e}"),
                        }));
                    }

                    // Store schema metadata
                    if let Err(e) = backend.store_schema_metadata(schema).await {
                        return Ok(json!({
                            "status": "error",
                            "schema": name,
                            "message": format!("failed to store metadata: {e}"),
                        }));
                    }

                    // Update registry cache
                    registry.insert(name.clone(), schema.clone()).await;

                    results.push(json!({
                        "schema": name,
                        "action": if existing.is_some() { "updated" } else { "created" },
                        "migration_steps": step_descriptions,
                    }));
                }
            }

            let status = if dry_run {
                "dry_run_complete"
            } else {
                "applied"
            };
            Ok(json!({
                "status": status,
                "results": results,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_acton::state::DynForgeBackend;
    use schema_forge_backend::error::BackendError;
    use schema_forge_backend::traits::{EntityStore, SchemaBackend};
    use schema_forge_core::migration::MigrationStep;
    use schema_forge_core::query::Query;
    use schema_forge_core::types::{
        EntityId, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
        TextConstraints,
    };

    /// Minimal mock backend for testing.
    struct MockBackend;

    impl SchemaBackend for MockBackend {
        async fn apply_migration(
            &self,
            _schema_name: &SchemaName,
            _steps: &[MigrationStep],
        ) -> Result<(), BackendError> {
            Ok(())
        }

        async fn store_schema_metadata(
            &self,
            _definition: &SchemaDefinition,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        async fn load_schema_metadata(
            &self,
            _name: &SchemaName,
        ) -> Result<Option<SchemaDefinition>, BackendError> {
            Ok(None)
        }

        async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
            Ok(vec![])
        }
    }

    impl EntityStore for MockBackend {
        async fn create(
            &self,
            _entity: &schema_forge_backend::Entity,
        ) -> Result<schema_forge_backend::Entity, BackendError> {
            Err(BackendError::Internal {
                message: "not implemented".into(),
            })
        }

        async fn get(
            &self,
            _schema: &SchemaName,
            _id: &EntityId,
        ) -> Result<schema_forge_backend::Entity, BackendError> {
            Err(BackendError::Internal {
                message: "not implemented".into(),
            })
        }

        async fn update(
            &self,
            _entity: &schema_forge_backend::Entity,
        ) -> Result<schema_forge_backend::Entity, BackendError> {
            Err(BackendError::Internal {
                message: "not implemented".into(),
            })
        }

        async fn delete(&self, _schema: &SchemaName, _id: &EntityId) -> Result<(), BackendError> {
            Ok(())
        }

        async fn query(
            &self,
            _query: &Query,
        ) -> Result<schema_forge_backend::QueryResult, BackendError> {
            Ok(schema_forge_backend::QueryResult {
                entities: vec![],
                total_count: Some(0),
            })
        }

        async fn count(&self, _query: &Query) -> Result<usize, BackendError> {
            Ok(0)
        }

        async fn aggregate(
            &self,
            _query: &schema_forge_core::query::AggregateQuery,
        ) -> Result<Vec<schema_forge_core::query::AggregateResult>, BackendError> {
            Ok(vec![])
        }
    }

    fn mock_backend() -> Arc<dyn DynForgeBackend> {
        Arc::new(MockBackend)
    }

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
    fn definition_has_correct_name_and_schema() {
        let def = apply_schema_tool_definition();
        assert_eq!(def.name, "apply_schema");
        let required = def.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("dsl")));
    }

    #[tokio::test]
    async fn parse_failure_returns_error_json() {
        let registry = SchemaRegistry::new();
        let executor = apply_schema_executor(registry, mock_backend());
        let result = executor(json!({"dsl": "schema { broken"})).await.unwrap();
        assert_eq!(result["status"], "parse_error");
    }

    #[tokio::test]
    async fn dry_run_returns_migration_plan() {
        let registry = SchemaRegistry::new();
        let executor = apply_schema_executor(registry, mock_backend());
        let result = executor(json!({
            "dsl": "schema Contact {\n    name: text required\n}",
            "dry_run": true
        }))
        .await
        .unwrap();
        assert_eq!(result["status"], "dry_run_complete");
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["action"], "create");
    }

    #[tokio::test]
    async fn new_schema_creates_and_registers() {
        let registry = SchemaRegistry::new();
        let executor = apply_schema_executor(registry.clone(), mock_backend());
        let result = executor(json!({
            "dsl": "schema Contact {\n    name: text required\n}"
        }))
        .await
        .unwrap();
        assert_eq!(result["status"], "applied");
        // Verify it was registered in the cache
        assert!(registry.get("Contact").await.is_some());
    }

    #[tokio::test]
    async fn existing_schema_diffs_and_updates() {
        let registry = SchemaRegistry::new();
        registry
            .insert("Contact".to_string(), make_test_schema("Contact"))
            .await;

        let executor = apply_schema_executor(registry.clone(), mock_backend());
        let result = executor(json!({
            "dsl": "schema Contact {\n    name: text required\n    email: text required\n}"
        }))
        .await
        .unwrap();
        assert_eq!(result["status"], "applied");
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["action"], "updated");
    }
}
