use std::sync::Arc;

use acton_ai::prelude::{ActonAI, ActonAIBuilder, Message};
use schema_forge_acton::state::{DynForgeBackend, ForgeState, SchemaRegistry};

use crate::error::ForgeAiError;
use crate::prompt::FORGE_SYSTEM_PROMPT;
use crate::tools::SchemaForgeTools;

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
    pub async fn generate(&self, description: &str) -> Result<String, ForgeAiError> {
        let mut builder = self.runtime.prompt(description).system(FORGE_SYSTEM_PROMPT);
        builder = self.tools.attach_to(builder);
        let response = builder
            .collect()
            .await
            .map_err(|e| ForgeAiError::runtime_error(e.to_string()))?;
        Ok(response.text)
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
}
