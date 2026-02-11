use std::fmt;
use std::sync::Arc;

use acton_ai::prelude::{ActonAI, ActonAIBuilder, Message, ProviderConfig};
use acton_ai::stream::ExecutedToolCall;
use schema_forge_acton::state::{DynForgeBackend, ForgeState, SchemaRegistry};

use crate::error::ForgeAiError;
use crate::prompt::FORGE_SYSTEM_PROMPT;
use crate::tools::{SchemaForgeTools, ValidatedDslCapture};

/// Maximum number of generation rounds beyond the initial attempt.
///
/// Each round either corrects invalid DSL or prompts for additional schemas.
/// With 5 rounds, the system can accumulate up to ~6 schemas (1 per round).
const MAX_GENERATION_ROUNDS: usize = 5;

/// Name of the syntax example schema in the system prompt.
/// Filtered out of generated results since small models copy it verbatim.
const EXAMPLE_SCHEMA_NAME: &str = "ExampleWidget";

/// Default max output tokens for LLM responses.
///
/// Schema generation with 5+ schemas, tool calls, and conversational text
/// can easily exceed 4096 tokens. 16384 gives ample headroom for complex
/// multi-schema generation workflows.
const DEFAULT_MAX_TOKENS: u32 = 16384;

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
///
/// Ordering: Registry (best) < ToolArguments < ResponseText < RawText (worst).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DslSource {
    /// Schemas applied via `apply_schema` tool and read back from the registry (best).
    Registry,
    /// DSL extracted from successful `validate_schema` or `apply_schema` tool call arguments.
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
        let config = ProviderConfig::ollama(model).with_max_tokens(DEFAULT_MAX_TOKENS);
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .provider_named("default", config);
        build_agent(builder, registry, backend).await
    }

    /// Build with Anthropic provider.
    pub async fn anthropic(
        api_key: impl Into<String>,
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let config = ProviderConfig::anthropic(api_key).with_max_tokens(DEFAULT_MAX_TOKENS);
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .provider_named("default", config);
        build_agent(builder, registry, backend).await
    }

    /// Build with OpenAI provider.
    pub async fn openai(
        api_key: impl Into<String>,
        registry: SchemaRegistry,
        backend: Arc<dyn DynForgeBackend>,
    ) -> Result<Self, ForgeAiError> {
        let config = ProviderConfig::openai(api_key).with_max_tokens(DEFAULT_MAX_TOKENS);
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .provider_named("default", config);
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
        let config = ProviderConfig::ollama(model).with_max_tokens(DEFAULT_MAX_TOKENS);
        let builder = ActonAI::builder()
            .app_name("schema-forge")
            .provider_named("default", config)
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
    /// After collection, extracts DSL from four sources in priority order:
    ///
    /// 1. **Registry** — schemas applied via `apply_schema` tool
    /// 2. **Tool arguments** — DSL from the last successful `validate_schema`/`apply_schema` call
    /// 3. **Response text** — parsed from the LLM's final text (including markdown code blocks)
    /// 4. **Raw text** — unparseable fallback
    ///
    /// If the initial attempt falls through to tier 4 (no valid DSL), enters a
    /// correction loop: extracts parse errors from the response text and sends
    /// them back to the model for up to [`MAX_GENERATION_ROUNDS`] rounds.
    ///
    /// Uses low temperature (0.3) for deterministic tool usage.
    pub async fn generate_dsl(&self, description: &str) -> Result<GenerateResult, ForgeAiError> {
        let registry = self.tools.registry();
        let capture = SchemaForgeTools::new_capture();

        // Accumulate schemas across multiple LLM rounds. Small models often
        // generate one schema per tool call and stop, requiring continuation
        // prompts to produce all requested schemas.
        let mut accumulated = Vec::<schema_forge_core::types::SchemaDefinition>::new();
        let mut best_source = DslSource::RawText;

        // Initial attempt — only attach generation tools (validate, apply, cedar)
        // to prevent small models from wasting rounds on list_schemas loops.
        // Allow 20 tool rounds since 5 schemas × (validate + apply) = 10+ calls.
        let mut builder = self
            .runtime
            .prompt(description)
            .system(FORGE_SYSTEM_PROMPT)
            .temperature(0.3)
            .max_tool_rounds(20);
        builder = self.tools.attach_generation_tools(builder, capture.clone());
        let response = match builder.collect().await {
            Ok(r) => r,
            Err(e) => {
                // Initial attempt failed (e.g. exceeded tool rounds).
                // Check if apply_schema was called before the error.
                let registry_schemas = registry.list().await;
                if !registry_schemas.is_empty() {
                    let dsl = schema_forge_dsl::print_all(&registry_schemas);
                    return Ok(GenerateResult {
                        schema_count: registry_schemas.len(),
                        dsl,
                        source: DslSource::Registry,
                        assistant_text: String::new(),
                    });
                }
                // Fall back to captured validated DSL from validate_schema calls
                if let Some(result) = recover_from_capture(&capture) {
                    return Ok(result);
                }
                return Err(ForgeAiError::runtime_error(e.to_string()));
            }
        };

        // Check registry first (Tier 1) — if apply_schema was called, we're done
        let registry_schemas = registry.list().await;
        if !registry_schemas.is_empty() {
            let dsl = schema_forge_dsl::print_all(&registry_schemas);
            return Ok(GenerateResult {
                schema_count: registry_schemas.len(),
                dsl,
                source: DslSource::Registry,
                assistant_text: response.text,
            });
        }

        // Collect schemas from this round's tool calls and response text
        let (new_schemas, source) =
            collect_schemas_from_round(&response.tool_calls, &response.text);
        merge_schemas(&mut accumulated, new_schemas);
        if source < best_source {
            best_source = source;
        }

        // Continuation loop: prompt for remaining schemas or fix errors
        let mut messages = vec![
            Message::user(description),
            Message::assistant(&response.text),
        ];
        let mut last_response_text = response.text;

        for _round in 0..MAX_GENERATION_ROUNDS {
            let feedback = if accumulated.is_empty() {
                // No schemas yet — send correction feedback
                build_correction_feedback(&last_response_text)
            } else {
                // Have some schemas — build continuation with missing names
                build_continuation_prompt(description, &accumulated)
            };
            messages.push(Message::user(&feedback));

            let mut retry_builder = self
                .runtime
                .continue_with(messages.clone())
                .system(FORGE_SYSTEM_PROMPT)
                .temperature(0.3)
                .max_tool_rounds(20);
            retry_builder = self
                .tools
                .attach_generation_tools(retry_builder, capture.clone());

            let retry_response = match retry_builder.collect().await {
                Ok(r) => r,
                Err(_) if !accumulated.is_empty() => {
                    // Round failed but we have schemas — return them
                    break;
                }
                Err(_) => {
                    // Try to recover from capture before giving up
                    if let Some(result) = recover_from_capture(&capture) {
                        return Ok(result);
                    }
                    break;
                }
            };

            // Check registry again
            let registry_schemas = registry.list().await;
            if !registry_schemas.is_empty() {
                let dsl = schema_forge_dsl::print_all(&registry_schemas);
                return Ok(GenerateResult {
                    schema_count: registry_schemas.len(),
                    dsl,
                    source: DslSource::Registry,
                    assistant_text: retry_response.text,
                });
            }

            let (new_schemas, source) =
                collect_schemas_from_round(&retry_response.tool_calls, &retry_response.text);

            messages.push(Message::assistant(&retry_response.text));
            last_response_text = retry_response.text.clone();

            if new_schemas.is_empty() && !accumulated.is_empty() {
                // We have some schemas and the model produced nothing new — stop
                break;
            }

            merge_schemas(&mut accumulated, new_schemas);
            if source < best_source {
                best_source = source;
            }
        }

        // Build final result from accumulated schemas
        if accumulated.is_empty() {
            Ok(GenerateResult {
                dsl: last_response_text.clone(),
                source: DslSource::RawText,
                assistant_text: last_response_text,
                schema_count: 0,
            })
        } else {
            let dsl = schema_forge_dsl::print_all(&accumulated);
            Ok(GenerateResult {
                schema_count: accumulated.len(),
                dsl,
                source: best_source,
                assistant_text: last_response_text,
            })
        }
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
/// 2. Tool arguments — DSL from successful `validate_schema`/`apply_schema`
/// 3. Response text — parsed from the LLM's conversational text
/// 4. Raw text — unparseable fallback
#[cfg(test)]
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

    // Tier 2: Tool arguments — aggregate schemas from ALL successful
    // validate_schema or apply_schema calls (model may call once per schema)
    {
        let mut all_schemas = Vec::new();
        for call in tool_calls {
            if (call.name == "validate_schema" || call.name == "apply_schema")
                && call.result.is_ok()
            {
                if let Some(dsl_str) = call.arguments["dsl"].as_str() {
                    if let Ok(parsed) = schema_forge_dsl::parse(dsl_str) {
                        all_schemas.extend(parsed);
                    }
                }
            }
        }
        if !all_schemas.is_empty() {
            let dsl = schema_forge_dsl::print_all(&all_schemas);
            return GenerateResult {
                schema_count: all_schemas.len(),
                dsl,
                source: DslSource::ToolArguments,
                assistant_text: response_text.to_string(),
            };
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

    // Try extracting from markdown code blocks (aggregate all parseable blocks)
    {
        let mut all_schemas = Vec::new();
        for block in extract_dsl_from_markdown(response_text) {
            if let Ok(parsed) = schema_forge_dsl::parse(&block) {
                all_schemas.extend(parsed);
            }
        }
        if !all_schemas.is_empty() {
            let dsl = schema_forge_dsl::print_all(&all_schemas);
            return GenerateResult {
                schema_count: all_schemas.len(),
                dsl,
                source: DslSource::ResponseText,
                assistant_text: response_text.to_string(),
            };
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

/// Collect parsed schemas from a single LLM round's tool calls and response text.
///
/// Returns the schemas found and the best extraction source tier.
/// Does NOT check the registry (that's handled separately in `generate_dsl`).
fn collect_schemas_from_round(
    tool_calls: &[ExecutedToolCall],
    response_text: &str,
) -> (Vec<schema_forge_core::types::SchemaDefinition>, DslSource) {
    // Tier 2: Tool arguments — aggregate from all successful validate/apply calls
    let mut from_tools = Vec::new();
    for call in tool_calls {
        if (call.name == "validate_schema" || call.name == "apply_schema") && call.result.is_ok() {
            if let Some(dsl_str) = call.arguments["dsl"].as_str() {
                if let Ok(parsed) = schema_forge_dsl::parse(dsl_str) {
                    from_tools.extend(parsed);
                }
            }
        }
    }

    // Tier 3: Response text — try full text, then markdown blocks
    let mut from_text = Vec::new();
    if let Ok(parsed) = schema_forge_dsl::parse(response_text) {
        from_text = parsed;
    }
    if from_text.is_empty() {
        for block in extract_dsl_from_markdown(response_text) {
            if let Ok(parsed) = schema_forge_dsl::parse(&block) {
                from_text.extend(parsed);
            }
        }
    }

    // Merge: prefer tool schemas, add any text-only schemas not already present
    let mut combined = if !from_tools.is_empty() {
        let tool_names: Vec<String> = from_tools.iter().map(|s| s.name.to_string()).collect();
        for s in from_text {
            if !tool_names.iter().any(|n| n == s.name.as_str()) {
                from_tools.push(s);
            }
        }
        from_tools
    } else {
        from_text
    };

    // Filter out the syntax example schema from the system prompt
    combined.retain(|s| s.name.as_str() != EXAMPLE_SCHEMA_NAME);

    let source = if !combined.is_empty() {
        if !tool_calls.is_empty() {
            DslSource::ToolArguments
        } else {
            DslSource::ResponseText
        }
    } else {
        DslSource::RawText
    };
    (combined, source)
}

/// Recover schemas from the validated DSL capture buffer.
///
/// When the tool loop is exhausted, `collect()` returns an error and discards
/// all tool call data. But the validate executor has been storing successfully
/// validated DSL in the capture buffer. This function parses those captured
/// strings, deduplicates, filters the example schema, and returns a result.
fn recover_from_capture(capture: &ValidatedDslCapture) -> Option<GenerateResult> {
    let captured = capture.lock().ok()?;
    if captured.is_empty() {
        return None;
    }

    let mut schemas = Vec::new();
    for dsl_str in captured.iter() {
        if let Ok(parsed) = schema_forge_dsl::parse(dsl_str) {
            for schema in parsed {
                if schema.name.as_str() != EXAMPLE_SCHEMA_NAME
                    && !schemas
                        .iter()
                        .any(|s: &schema_forge_core::types::SchemaDefinition| s.name == schema.name)
                {
                    schemas.push(schema);
                }
            }
        }
    }

    if schemas.is_empty() {
        return None;
    }

    let dsl = schema_forge_dsl::print_all(&schemas);
    Some(GenerateResult {
        schema_count: schemas.len(),
        dsl,
        source: DslSource::ToolArguments,
        assistant_text: String::new(),
    })
}

/// Merge new schemas into the accumulated set, skipping duplicates by name.
fn merge_schemas(
    accumulated: &mut Vec<schema_forge_core::types::SchemaDefinition>,
    new_schemas: Vec<schema_forge_core::types::SchemaDefinition>,
) {
    for schema in new_schemas {
        if !accumulated.iter().any(|s| s.name == schema.name) {
            accumulated.push(schema);
        }
    }
}

/// Build a continuation prompt that tells the model which schemas are still missing.
///
/// Extracts PascalCase words from the original description as candidate schema names,
/// compares with already-generated schemas, and asks for specific missing ones.
fn build_continuation_prompt(
    description: &str,
    accumulated: &[schema_forge_core::types::SchemaDefinition],
) -> String {
    let generated_names: Vec<&str> = accumulated.iter().map(|s| s.name.as_str()).collect();

    // Extract PascalCase words from the description as candidate schema names
    let expected: Vec<&str> = description
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| {
            w.len() >= 2
                && w.chars().next().is_some_and(|c| c.is_uppercase())
                && w.chars().skip(1).any(|c| c.is_lowercase())
                && !is_common_word(w)
        })
        .collect();

    let missing: Vec<&&str> = expected
        .iter()
        .filter(|name| !generated_names.iter().any(|g| g == *name))
        .collect();

    if missing.is_empty() {
        format!(
            "You have generated: {}. The original request was: \"{}\". \
             If you believe more schemas are needed, generate the next one \
             using `validate_schema`. Otherwise you are done.",
            generated_names.join(", "),
            description
        )
    } else {
        format!(
            "You have generated: {}. But the request also needs: {}. \
             Generate the `{}` schema now. Call `validate_schema` with the DSL.",
            generated_names.join(", "),
            missing
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", "),
            missing[0]
        )
    }
}

/// Returns true for common English words that happen to start with uppercase.
fn is_common_word(word: &str) -> bool {
    matches!(
        word,
        "The"
            | "This"
            | "These"
            | "Those"
            | "They"
            | "There"
            | "Their"
            | "Then"
            | "Than"
            | "Include"
            | "Including"
            | "Create"
            | "Design"
            | "Build"
            | "Each"
            | "All"
            | "Some"
            | "Any"
            | "Every"
            | "Both"
            | "Products"
            | "Categories"
            | "Customers"
            | "Orders"
            | "Reviews"
            | "Items"
            | "Users"
            | "Posts"
            | "Comments"
            | "Tags"
            | "Has"
            | "Have"
            | "Had"
            | "Does"
            | "Did"
            | "Will"
            | "Would"
            | "Can"
            | "Could"
            | "Should"
            | "May"
            | "Might"
            | "Must"
            | "Are"
            | "Were"
            | "Was"
            | "Been"
            | "Being"
            | "Not"
            | "But"
            | "And"
            | "For"
            | "With"
            | "From"
            | "Into"
            | "About"
            | "After"
            | "Before"
            | "Between"
            | "Through"
            | "During"
            | "Without"
            | "Within"
            | "Along"
            | "Among"
            | "Upon"
            | "Against"
            | "Across"
            | "Behind"
            | "Beyond"
    )
}

/// Build a correction feedback message from the model's response text.
///
/// Extracts DSL from markdown code blocks, attempts to parse each block,
/// and constructs feedback with specific parse errors when available.
/// Falls back to generic tool-usage instructions when no DSL-like content is found.
fn build_correction_feedback(response_text: &str) -> String {
    let blocks = extract_dsl_from_markdown(response_text);

    let mut all_errors = Vec::new();
    for block in &blocks {
        if let Err(errors) = schema_forge_dsl::parse(block) {
            for err in &errors {
                all_errors.push(err.to_string());
            }
        }
    }

    if !all_errors.is_empty() {
        format!(
            "Your DSL has parse errors. Please fix these errors and call the \
             `validate_schema` tool with the corrected DSL:\n\n{}\n\n\
             Remember: schema names must be PascalCase, field names must be \
             snake_case, and all types must match the grammar exactly.",
            all_errors.join("\n")
        )
    } else {
        "Your response did not contain valid SchemaDSL. Please generate the schema \
         using the `validate_schema` tool to ensure correctness, then call \
         `apply_schema` to register it. Do NOT just write DSL in text — you MUST \
         use the tools."
            .to_string()
    }
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
    async fn extract_dsl_tier2_aggregates_multiple_calls() {
        let registry = SchemaRegistry::new();
        let dsl_v1 = "schema Product {\n    name: text\n}";
        let dsl_v2 = "schema Category {\n    name: text required\n}";
        let tool_calls = vec![
            make_successful_tool_call("validate_schema", dsl_v1),
            make_successful_tool_call("validate_schema", dsl_v2),
        ];

        let result = extract_dsl(&registry, &tool_calls, "Done!").await;
        assert_eq!(result.source, DslSource::ToolArguments);
        assert_eq!(result.schema_count, 2);
        assert!(result.dsl.contains("Product"));
        assert!(result.dsl.contains("Category"));
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
    async fn extract_dsl_tier3_aggregates_markdown_blocks() {
        let registry = SchemaRegistry::new();
        let text = "First schema:\n\n```schemadsl\nschema Product {\n    name: text required\n}\n```\n\nSecond schema:\n\n```schemadsl\nschema Category {\n    title: text required\n}\n```\n\nDone!";

        let result = extract_dsl(&registry, &[], text).await;
        assert_eq!(result.source, DslSource::ResponseText);
        assert_eq!(result.schema_count, 2);
        assert!(result.dsl.contains("Product"));
        assert!(result.dsl.contains("Category"));
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
        let text =
            "```\nschema A { name: text }\n```\nsome text\n```\nschema B { age: integer }\n```";
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

    // -- build_correction_feedback tests --

    #[test]
    fn correction_feedback_includes_parse_errors() {
        // Uppercase field name → parser should produce an error
        let text = "Here is the schema:\n\n```schemadsl\nschema Book {\n    Title: text\n}\n```";
        let feedback = build_correction_feedback(text);
        assert!(
            feedback.contains("parse error") || feedback.contains("PascalCase"),
            "feedback should contain parse errors: {feedback}"
        );
        assert!(feedback.contains("validate_schema"));
    }

    #[test]
    fn correction_feedback_without_dsl_content() {
        let text = "I created a wonderful schema for you! It has books and authors.";
        let feedback = build_correction_feedback(text);
        assert!(feedback.contains("validate_schema"));
        assert!(feedback.contains("MUST"));
    }

    #[test]
    fn correction_feedback_with_empty_schema() {
        // Empty schema body → parser produces EmptySchema error
        let text = "```\nschema Empty {}\n```";
        let feedback = build_correction_feedback(text);
        assert!(
            feedback.contains("no fields") || feedback.contains("parse error"),
            "feedback should mention empty schema: {feedback}"
        );
    }

    #[test]
    fn correction_feedback_valid_dsl_returns_generic() {
        // Valid DSL in a markdown block. In practice, extract_dsl would
        // catch this before build_correction_feedback is called, but if it
        // does get called, the generic message should be returned since
        // there are no parse errors to report.
        let text = "```\nschema Contact {\n    name: text required\n}\n```";
        let feedback = build_correction_feedback(text);
        // No errors extracted → generic message
        assert!(feedback.contains("validate_schema"));
    }

    #[test]
    fn max_generation_rounds_constant() {
        assert_eq!(MAX_GENERATION_ROUNDS, 5);
    }

    // -- recover_from_capture tests --

    #[test]
    fn recover_from_empty_capture_returns_none() {
        let capture = SchemaForgeTools::new_capture();
        assert!(recover_from_capture(&capture).is_none());
    }

    #[test]
    fn recover_from_capture_with_valid_dsl() {
        let capture = SchemaForgeTools::new_capture();
        capture
            .lock()
            .unwrap()
            .push("schema Product {\n    name: text required\n}".to_string());

        let result = recover_from_capture(&capture).unwrap();
        assert_eq!(result.source, DslSource::ToolArguments);
        assert_eq!(result.schema_count, 1);
        assert!(result.dsl.contains("Product"));
    }

    #[test]
    fn recover_from_capture_deduplicates() {
        let capture = SchemaForgeTools::new_capture();
        {
            let mut buf = capture.lock().unwrap();
            buf.push("schema Product {\n    name: text\n}".to_string());
            buf.push("schema Product {\n    name: text required\n}".to_string());
            buf.push("schema Category {\n    title: text\n}".to_string());
        }

        let result = recover_from_capture(&capture).unwrap();
        assert_eq!(result.schema_count, 2); // Product + Category, not 3
    }

    #[test]
    fn recover_from_capture_filters_example_widget() {
        let capture = SchemaForgeTools::new_capture();
        {
            let mut buf = capture.lock().unwrap();
            buf.push("schema ExampleWidget {\n    label: text\n}".to_string());
        }

        assert!(recover_from_capture(&capture).is_none());
    }

    #[test]
    fn recover_from_capture_skips_invalid_dsl() {
        let capture = SchemaForgeTools::new_capture();
        {
            let mut buf = capture.lock().unwrap();
            buf.push("schema { broken".to_string());
            buf.push("schema Valid {\n    name: text\n}".to_string());
        }

        let result = recover_from_capture(&capture).unwrap();
        assert_eq!(result.schema_count, 1);
        assert!(result.dsl.contains("Valid"));
    }
}
