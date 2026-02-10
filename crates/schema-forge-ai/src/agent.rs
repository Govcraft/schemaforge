use std::fmt;
use std::sync::Arc;

use acton_ai::prelude::{ActonAI, ActonAIBuilder, Message};
use acton_ai::stream::ExecutedToolCall;
use schema_forge_acton::state::{DynForgeBackend, ForgeState, SchemaRegistry};

use crate::error::ForgeAiError;
use crate::prompt::FORGE_SYSTEM_PROMPT;
use crate::tools::SchemaForgeTools;

/// The result of `generate_dsl()`, containing validated DSL and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateResult {
    /// The extracted DSL source text.
    pub dsl: String,
    /// Where the DSL was extracted from.
    pub source: DslSource,
    /// The LLM's final conversational text.
    pub assistant_text: String,
    /// Number of schemas found in the DSL.
    pub schema_count: usize,
}

/// Indicates where the DSL was extracted from, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DslSource {
    /// Schemas applied via `apply_schema` tool and read back from the registry (best).
    Registry,
    /// DSL extracted from the last successful `validate_schema` or `apply_schema` tool call arguments.
    ToolArguments,
    /// Parsed from the LLM's response text (including markdown code blocks).
    ResponseText,
    /// Unparseable fallback — raw LLM response text.
    RawText,
}

impl fmt::Display for DslSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DslSource::Registry => write!(f, "registry"),
            DslSource::ToolArguments => write!(f, "tool_arguments"),
            DslSource::ResponseText => write!(f, "response_text"),
            DslSource::RawText => write!(f, "raw_text"),
        }
    }
}

/// The SchemaForge AI agent, wrapping an `ActonAI` runtime with
/// schema-specific tools and system prompt.
pub struct SchemaForgeAgent {
    runtime: ActonAI,
    tools: SchemaForgeTools,
}

impl SchemaForgeAgent {
    /// Returns a `ForgeState` for use in HTTP handlers.
    pub fn forge_state(&self) -> ForgeState {
        ForgeState {
            registry: self.tools.registry().clone(),
            backend: self.tools.backend().clone(),
        }
    }

    /// Build with Ollama provider.
    pub async fn ollama(
        model: impl Into<String>,
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let builder = ActonAI::builder().app_name("schema-forge").ollama(model);
        build_agent(builder, registry, backend).await
    }

    /// Build with Anthropic provider.
    pub async fn anthropic(
        api_key: impl Into<String>,
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .anthropic(api_key);
        build_agent(builder, registry, backend).await
    }

    /// Build with OpenAI provider.
    pub async fn openai(
        api_key: impl Into<String>,
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let builder = ActonAI::builder().app_name("schema-forge").openai(api_key);
        build_agent(builder, registry, backend).await
    }

    /// Build from acton-ai config file (acton-ai.toml).
    pub async fn from_config(
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .from_config()
            .map_err(ForgeAiError::from)?;
        build_agent(builder, registry, backend).await
    }

    /// Build with Ollama provider and pre-configured file-access builtins.
    pub async fn with_builtins(
        model: impl Into<String>,
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .ollama(model)
            .with_builtins();
        build_agent(builder, registry, backend).await
    }

    /// Single-shot generation: returns the final text response.
    ///
    /// Sends the description to the LLM with the SchemaForge system prompt
    /// and all tools attached. The LLM may call tools during generation.
    /// Uses low temperature (0.3) for deterministic tool usage.
    pub async fn generate(&self, description: &str) -> Result<String, ForgeAiError> {
        let mut builder = self
            .runtime
            .prompt(description)
            .system(FORGE_SYSTEM_PROMPT)
            .temperature(0.3);
        builder = self.tools.attach_to(builder);
        let response = builder
            .collect()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))?;
        Ok(response.text)
    }

    /// Single-shot generation that reliably extracts validated DSL.
    ///
    /// Like `generate()`, sends the description to the LLM with tools attached.
    /// After collection, extracts DSL from three sources in priority order:
    ///
    /// 1. **Registry** — schemas applied via `apply_schema` tool
    /// 2. **Tool arguments** — DSL from the last successful `validate_schema`/`apply_schema` call
    /// 3. **Response text** — parsed from the LLM's final text (including markdown code blocks)
    /// 4. **Raw text** — unparseable fallback
    ///
    /// Uses low temperature (0.3) for deterministic tool usage.
    pub async fn generate_dsl(&self, description: &str) -> Result<GenerateResult, ForgeAiError> {
        let mut builder = self
            .runtime
            .prompt(description)
            .system(FORGE_SYSTEM_PROMPT)
            .temperature(0.3);
        builder = self.tools.attach_to(builder);
        let response = builder
            .collect()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))?;

        let registry = self.tools.registry();
        Ok(extract_dsl(registry, &response.tool_calls, &response.text).await)
    }

    /// Single-shot with streaming tokens via callback.
    pub async fn generate_streaming(
        &self,
        description: &str,
        on_token: impl FnMut(&str) + Send + 'static,
    ) -> Result<String, ForgeAiError> {
        let mut builder = self
            .runtime
            .prompt(description)
            .system(FORGE_SYSTEM_PROMPT)
            .on_token(on_token);
        builder = self.tools.attach_to(builder);
        let response = builder
            .collect()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))?;
        Ok(response.text)
    }

    /// Single-shot with a specific named provider.
    pub async fn generate_with_provider(
        &self,
        description: &str,
        provider_name: &str,
    ) -> Result<String, ForgeAiError> {
        let mut builder = self
            .runtime
            .prompt(description)
            .system(FORGE_SYSTEM_PROMPT)
            .provider(provider_name);
        builder = self.tools.attach_to(builder);
        let response = builder
            .collect()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))?;
        Ok(response.text)
    }

    /// Continue a conversation from existing message history.
    ///
    /// Uses `continue_with()` to pass the full conversation history on each turn,
    /// and attaches tools via `SchemaForgeTools::attach_to()` on every prompt.
    pub async fn continue_conversation(
        &self,
        messages: Vec<Message>,
    ) -> Result<String, ForgeAiError> {
        let mut builder = self
            .runtime
            .continue_with(messages)
            .system(FORGE_SYSTEM_PROMPT);
        builder = self.tools.attach_to(builder);
        let response = builder
            .collect()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))?;
        Ok(response.text)
    }

    /// Returns a reference to the underlying `ActonAI` runtime.
    pub fn runtime(&self) -> &ActonAI {
        &self.runtime
    }

    /// Returns a reference to the tools.
    pub fn tools(&self) -> &SchemaForgeTools {
        &self.tools
    }

    /// Shutdown the agent runtime.
    pub async fn shutdown(self) -> Result<(), ForgeAiError> {
        self.runtime
            .shutdown()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))
    }
}

/// Internal helper to launch the runtime and wrap it in a `SchemaForgeAgent`.
async fn build_agent(
    builder: ActonAIBuilder,
    registry: SchemaRegistry,
    backend: Arc<dyn DynForgeBackend>,
) -> Result<SchemaForgeAgent, ForgeAiError> {
    let runtime = builder.launch().await.map_err(ForgeAiError::from)?;
    let tools = SchemaForgeTools::new(registry, backend);
    Ok(SchemaForgeAgent { runtime, tools })
}

/// Extract validated DSL from the response using a priority waterfall.
///
/// Tries each tier in order and returns the first successful extraction:
/// 1. Registry — schemas applied via `apply_schema`
/// 2. Tool arguments — DSL from last successful `validate_schema`/`apply_schema`
/// 3. Response text — parsed from the LLM's conversational text
/// 4. Raw text — unparseable fallback
pub(crate) async fn extract_dsl(
    registry: &SchemaRegistry,
    tool_calls: &[ExecutedToolCall],
    response_text: &str,
) -> GenerateResult {
    // Tier 1: Registry — schemas applied via apply_schema tool
    let schemas = registry.list().await;
    if !schemas.is_empty() {
        let dsl = schema_forge_dsl::print_all(&schemas);
        return GenerateResult {
            schema_count: schemas.len(),
            dsl,
            source: DslSource::Registry,
            assistant_text: response_text.to_string(),
        };
    }

    // Tier 2: Tool arguments — scan tool calls in reverse for last successful
    // validate_schema or apply_schema with a "dsl" argument
    for call in tool_calls.iter().rev() {
        if (call.name == "validate_schema" || call.name == "apply_schema")
            && call.result.is_ok()
        {
            if let Some(dsl_str) = call.arguments["dsl"].as_str() {
                if let Ok(parsed) = schema_forge_dsl::parse(dsl_str) {
                    if !parsed.is_empty() {
                        let dsl = schema_forge_dsl::print_all(&parsed);
                        return GenerateResult {
                            schema_count: parsed.len(),
                            dsl,
                            source: DslSource::ToolArguments,
                            assistant_text: response_text.to_string(),
                        };
                    }
                }
            }
        }
    }

    // Tier 3: Response text — try parsing the full response, then markdown blocks
    if let Ok(parsed) = schema_forge_dsl::parse(response_text) {
        if !parsed.is_empty() {
            let dsl = schema_forge_dsl::print_all(&parsed);
            return GenerateResult {
                schema_count: parsed.len(),
                dsl,
                source: DslSource::ResponseText,
                assistant_text: response_text.to_string(),
            };
        }
    }

    // Try extracting from markdown code blocks
    for block in extract_dsl_from_markdown(response_text) {
        if let Ok(parsed) = schema_forge_dsl::parse(&block) {
            if !parsed.is_empty() {
                let dsl = schema_forge_dsl::print_all(&parsed);
                return GenerateResult {
                    schema_count: parsed.len(),
                    dsl,
                    source: DslSource::ResponseText,
                    assistant_text: response_text.to_string(),
                };
            }
        }
    }

    // Tier 4: Raw text fallback
    GenerateResult {
        dsl: response_text.to_string(),
        source: DslSource::RawText,
        assistant_text: response_text.to_string(),
        schema_count: 0,
    }
}

/// Extract code blocks from markdown-formatted text.
///
/// Looks for fenced code blocks (triple backticks) with optional language tags
/// like `schemadsl`, `schema`, or no tag. Returns the content of each block.
pub(crate) fn extract_dsl_from_markdown(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current_block = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_block {
                // End of block
                if !current_block.trim().is_empty() {
                    blocks.push(current_block.trim().to_string());
                }
                current_block.clear();
                in_block = false;
            } else {
                // Start of block — accept any language tag or none
                in_block = true;
                current_block.clear();
            }
        } else if in_block {
            current_block.push_str(line);
            current_block.push('\n');
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_forge_agent_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SchemaForgeAgent>();
    }

    // Note: ActonAI is Send but not Sync (contains Arc<ActonAIInner> which has
    // interior mutability via AtomicBool), so SchemaForgeAgent is Send but
    // we only test Send here. The agent is designed for owned usage patterns.

    #[test]
    fn forge_state_is_clone_send_sync() {
        fn assert_clone_send_sync<T: Clone + Send + Sync>() {}
        assert_clone_send_sync::<ForgeState>();
    }

    #[test]
    fn generate_result_is_clone_eq() {
        let result = GenerateResult {
            dsl: "schema X { name: text }".to_string(),
            source: DslSource::Registry,
            assistant_text: "Done".to_string(),
            schema_count: 1,
        };
        let cloned = result.clone();
        assert_eq!(result, cloned);
    }

    #[test]
    fn dsl_source_display() {
        assert_eq!(DslSource::Registry.to_string(), "registry");
        assert_eq!(DslSource::ToolArguments.to_string(), "tool_arguments");
        assert_eq!(DslSource::ResponseText.to_string(), "response_text");
        assert_eq!(DslSource::RawText.to_string(), "raw_text");
    }

    // -- extract_dsl tests --

    const VALID_DSL: &str = "schema Contact {\n    name: text required\n}";

    fn make_successful_tool_call(name: &str, dsl: &str) -> ExecutedToolCall {
        ExecutedToolCall::success(
            "call_1",
            name,
            serde_json::json!({"dsl": dsl}),
            serde_json::json!({"status": "valid"}),
        )
    }

    fn make_failed_tool_call(name: &str, dsl: &str) -> ExecutedToolCall {
        ExecutedToolCall::error(
            "call_1",
            name,
            serde_json::json!({"dsl": dsl}),
            "parse error",
        )
    }

    #[tokio::test]
    async fn extract_dsl_tier1_registry_populated() {
        use schema_forge_core::types::{
            FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
            TextConstraints,
        };

        let registry = SchemaRegistry::new();
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();
        registry.insert("Contact".to_string(), schema).await;

        let result = extract_dsl(&registry, &[], "some text").await;
        assert_eq!(result.source, DslSource::Registry);
        assert_eq!(result.schema_count, 1);
        assert!(result.dsl.contains("Contact"));
    }

    #[tokio::test]
    async fn extract_dsl_tier2_tool_arguments() {
        let registry = SchemaRegistry::new();
        let tool_calls = vec![make_successful_tool_call("validate_schema", VALID_DSL)];

        let result = extract_dsl(&registry, &tool_calls, "Done!").await;
        assert_eq!(result.source, DslSource::ToolArguments);
        assert_eq!(result.schema_count, 1);
        assert!(result.dsl.contains("Contact"));
    }

    #[tokio::test]
    async fn extract_dsl_tier2_apply_schema_tool() {
        let registry = SchemaRegistry::new();
        let tool_calls = vec![make_successful_tool_call("apply_schema", VALID_DSL)];

        let result = extract_dsl(&registry, &tool_calls, "Done!").await;
        assert_eq!(result.source, DslSource::ToolArguments);
        assert_eq!(result.schema_count, 1);
    }

    #[tokio::test]
    async fn extract_dsl_tier2_skips_failed_tool_calls() {
        let registry = SchemaRegistry::new();
        let tool_calls = vec![make_failed_tool_call("validate_schema", VALID_DSL)];

        let result = extract_dsl(&registry, &tool_calls, "Done!").await;
        assert_eq!(result.source, DslSource::RawText);
    }

    #[tokio::test]
    async fn extract_dsl_tier2_uses_last_successful_call() {
        let registry = SchemaRegistry::new();
        let dsl_v1 = "schema OldSchema {\n    name: text\n}";
        let dsl_v2 = "schema NewSchema {\n    name: text required\n}";
        let tool_calls = vec![
            make_successful_tool_call("validate_schema", dsl_v1),
            make_successful_tool_call("validate_schema", dsl_v2),
        ];

        let result = extract_dsl(&registry, &tool_calls, "Done!").await;
        assert_eq!(result.source, DslSource::ToolArguments);
        // Should use the last (v2) since we scan in reverse
        assert!(result.dsl.contains("NewSchema"));
    }

    #[tokio::test]
    async fn extract_dsl_tier3_parseable_response_text() {
        let registry = SchemaRegistry::new();
        let result = extract_dsl(&registry, &[], VALID_DSL).await;
        assert_eq!(result.source, DslSource::ResponseText);
        assert_eq!(result.schema_count, 1);
        assert!(result.dsl.contains("Contact"));
    }

    #[tokio::test]
    async fn extract_dsl_tier3_markdown_code_block() {
        let registry = SchemaRegistry::new();
        let text = format!(
            "Here is the schema:\n\n```schemadsl\n{}\n```\n\nLet me know if you need changes.",
            VALID_DSL
        );

        let result = extract_dsl(&registry, &[], &text).await;
        assert_eq!(result.source, DslSource::ResponseText);
        assert_eq!(result.schema_count, 1);
        assert!(result.dsl.contains("Contact"));
    }

    #[tokio::test]
    async fn extract_dsl_tier4_unparseable_text() {
        let registry = SchemaRegistry::new();
        let result = extract_dsl(&registry, &[], "I created a great schema!").await;
        assert_eq!(result.source, DslSource::RawText);
        assert_eq!(result.schema_count, 0);
        assert_eq!(result.dsl, "I created a great schema!");
    }

    // -- extract_dsl_from_markdown tests --

    #[test]
    fn markdown_extracts_single_block() {
        let text = "Here:\n```\nschema X {\n    name: text\n}\n```\nDone.";
        let blocks = extract_dsl_from_markdown(text);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("schema X"));
    }

    #[test]
    fn markdown_extracts_with_language_tag() {
        let text = "```schemadsl\nschema Y {\n    age: integer\n}\n```";
        let blocks = extract_dsl_from_markdown(text);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("schema Y"));
    }

    #[test]
    fn markdown_extracts_multiple_blocks() {
        let text = "```\nschema A { name: text }\n```\nsome text\n```\nschema B { age: integer }\n```";
        let blocks = extract_dsl_from_markdown(text);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn markdown_skips_empty_blocks() {
        let text = "```\n\n```\nmore text";
        let blocks = extract_dsl_from_markdown(text);
        assert!(blocks.is_empty());
    }

    #[test]
    fn markdown_no_blocks_returns_empty() {
        let text = "No code blocks here, just text.";
        let blocks = extract_dsl_from_markdown(text);
        assert!(blocks.is_empty());
    }
}
