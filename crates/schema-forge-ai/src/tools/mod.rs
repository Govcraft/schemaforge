pub mod apply;
pub mod cedar;
pub mod list;
pub mod read_schema;
pub mod validate;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use acton_ai::prelude::{ToolDefinition, ToolError};
use schema_forge_acton::state::{DynForgeBackend, SchemaRegistry};
use serde_json::Value;

pub use validate::ValidatedDslCapture;

/// Holds all SchemaForge tool definitions and executors.
///
/// Attach to a `PromptBuilder` via `attach_to()` to make all tools
/// available to the LLM.
pub struct SchemaForgeTools {
    registry: SchemaRegistry,
    backend: Arc<dyn DynForgeBackend>,
}

/// Type alias for boxed tool executor closures.
type BoxedToolExecutor = Box<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>> + Send + Sync,
>;

impl SchemaForgeTools {
    /// Creates a new `SchemaForgeTools` with the given registry and backend.
    pub fn new(registry: SchemaRegistry, backend: Arc<dyn DynForgeBackend>) -> Self {
        Self { registry, backend }
    }

    /// Returns a reference to the schema registry.
    pub fn registry(&self) -> &SchemaRegistry {
        &self.registry
    }

    /// Returns a reference to the backend.
    pub fn backend(&self) -> &Arc<dyn DynForgeBackend> {
        &self.backend
    }

    /// Attach all SchemaForge tools to a `PromptBuilder`.
    ///
    /// This registers all 5 tools on the builder so the LLM can invoke them.
    pub fn attach_to(
        &self,
        builder: acton_ai::prompt::PromptBuilder,
    ) -> acton_ai::prompt::PromptBuilder {
        builder
            .with_tool(
                validate::validate_schema_tool_definition(),
                validate::validate_schema_executor(),
            )
            .with_tool(
                list::list_schemas_tool_definition(),
                list::list_schemas_executor(self.registry.clone()),
            )
            .with_tool(
                apply::apply_schema_tool_definition(),
                apply::apply_schema_executor(self.registry.clone(), self.backend.clone()),
            )
            .with_tool(
                cedar::generate_cedar_tool_definition(),
                cedar::generate_cedar_executor(self.registry.clone()),
            )
            .with_tool(
                read_schema::read_schema_file_tool_definition(),
                read_schema::read_schema_file_executor(),
            )
    }

    /// Create a new capture buffer for validated DSL strings.
    pub fn new_capture() -> ValidatedDslCapture {
        Arc::new(Mutex::new(Vec::new()))
    }

    /// Attach only generation-relevant tools to a `PromptBuilder`.
    ///
    /// Excludes `list_schemas` and `read_schema_file` which cause small models
    /// to waste tool rounds. Only includes `validate_schema`, `apply_schema`,
    /// and `generate_cedar`.
    ///
    /// The `capture` buffer receives successfully validated DSL strings from
    /// `validate_schema`, enabling recovery when the tool loop is exhausted.
    pub fn attach_generation_tools(
        &self,
        builder: acton_ai::prompt::PromptBuilder,
        capture: ValidatedDslCapture,
    ) -> acton_ai::prompt::PromptBuilder {
        builder
            .with_tool(
                validate::validate_schema_tool_definition(),
                validate::validate_schema_executor_with_capture(Some(capture)),
            )
            .with_tool(
                apply::apply_schema_tool_definition(),
                apply::apply_schema_executor(self.registry.clone(), self.backend.clone()),
            )
            .with_tool(
                cedar::generate_cedar_tool_definition(),
                cedar::generate_cedar_executor(self.registry.clone()),
            )
    }

    /// Returns the tool definitions for all 5 tools.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        vec![
            validate::validate_schema_tool_definition(),
            list::list_schemas_tool_definition(),
            apply::apply_schema_tool_definition(),
            cedar::generate_cedar_tool_definition(),
            read_schema::read_schema_file_tool_definition(),
        ]
    }

    /// Returns the tool executors paired with definitions.
    ///
    /// Useful for manual tool registration outside of the `PromptBuilder` pattern.
    pub fn executors(&self) -> Vec<(ToolDefinition, BoxedToolExecutor)> {
        vec![
            (
                validate::validate_schema_tool_definition(),
                Box::new(validate::validate_schema_executor()),
            ),
            (
                list::list_schemas_tool_definition(),
                Box::new(list::list_schemas_executor(self.registry.clone())),
            ),
            (
                apply::apply_schema_tool_definition(),
                Box::new(apply::apply_schema_executor(
                    self.registry.clone(),
                    self.backend.clone(),
                )),
            ),
            (
                cedar::generate_cedar_tool_definition(),
                Box::new(cedar::generate_cedar_executor(self.registry.clone())),
            ),
            (
                read_schema::read_schema_file_tool_definition(),
                Box::new(read_schema::read_schema_file_executor()),
            ),
        ]
    }

    /// Returns the number of tools provided.
    pub fn tool_count(&self) -> usize {
        5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_backend::error::BackendError;
    use schema_forge_backend::traits::{EntityStore, SchemaBackend};
    use schema_forge_core::migration::MigrationStep;
    use schema_forge_core::query::Query;
    use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};

    struct MockBackend;

    impl SchemaBackend for MockBackend {
        async fn apply_migration(
            &self,
            _name: &SchemaName,
            _steps: &[MigrationStep],
        ) -> Result<(), BackendError> {
            Ok(())
        }
        async fn store_schema_metadata(&self, _def: &SchemaDefinition) -> Result<(), BackendError> {
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
            _e: &schema_forge_backend::Entity,
        ) -> Result<schema_forge_backend::Entity, BackendError> {
            Err(BackendError::Internal {
                message: "mock".into(),
            })
        }
        async fn get(
            &self,
            _s: &SchemaName,
            _id: &EntityId,
        ) -> Result<schema_forge_backend::Entity, BackendError> {
            Err(BackendError::Internal {
                message: "mock".into(),
            })
        }
        async fn update(
            &self,
            _e: &schema_forge_backend::Entity,
        ) -> Result<schema_forge_backend::Entity, BackendError> {
            Err(BackendError::Internal {
                message: "mock".into(),
            })
        }
        async fn delete(&self, _s: &SchemaName, _id: &EntityId) -> Result<(), BackendError> {
            Ok(())
        }
        async fn query(
            &self,
            _q: &Query,
        ) -> Result<schema_forge_backend::QueryResult, BackendError> {
            Ok(schema_forge_backend::QueryResult {
                entities: vec![],
                total_count: Some(0),
            })
        }
    }

    fn make_tools() -> SchemaForgeTools {
        let registry = SchemaRegistry::new();
        let backend: Arc<dyn DynForgeBackend> = Arc::new(MockBackend);
        SchemaForgeTools::new(registry, backend)
    }

    #[test]
    fn tool_count_returns_five() {
        let tools = make_tools();
        assert_eq!(tools.tool_count(), 5);
    }

    #[test]
    fn definitions_returns_five_with_correct_names() {
        let tools = make_tools();
        let defs = tools.definitions();
        assert_eq!(defs.len(), 5);

        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"validate_schema"));
        assert!(names.contains(&"list_schemas"));
        assert!(names.contains(&"apply_schema"));
        assert!(names.contains(&"generate_cedar"));
        assert!(names.contains(&"read_schema_file"));
    }

    #[test]
    fn all_definition_names_are_unique() {
        let tools = make_tools();
        let defs = tools.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "duplicate tool names found");
    }
}
