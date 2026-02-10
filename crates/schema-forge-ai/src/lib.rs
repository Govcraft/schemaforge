pub mod agent;
pub mod cli;
pub mod endpoint;
pub mod error;
pub mod prompt;
pub mod tools;

// Re-exports for convenience
pub use agent::{DslSource, GenerateResult, SchemaForgeAgent};
pub use cli::run_interactive_cli;
pub use endpoint::{generate_handler, GenerateRequest, GenerateResponse};
pub use error::ForgeAiError;
pub use prompt::FORGE_SYSTEM_PROMPT;
pub use tools::SchemaForgeTools;
