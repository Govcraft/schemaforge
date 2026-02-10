use crate::cli::{GenerateArgs, GlobalOpts};
use crate::error::CliError;
use crate::output::OutputContext;

/// Run the `generate` command (stub).
///
/// Full implementation depends on acton-ai runtime being configured
/// with an appropriate AI provider (ollama, anthropic, openai).
pub async fn run(
    _args: GenerateArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.warn(
        "The 'generate' command requires AI provider configuration.\n\
         \n\
         To set up AI-powered schema generation:\n\
         \n\
         1. Create an acton-ai.toml file with your provider settings:\n\
         \n\
         [provider]\n\
         name = \"ollama\"           # or \"anthropic\", \"openai\"\n\
         model = \"llama3.1:8b\"     # model name\n\
         \n\
         2. For cloud providers, set your API key:\n\
         \n\
         export ANTHROPIC_API_KEY=sk-...\n\
         export OPENAI_API_KEY=sk-...\n\
         \n\
         3. Run again:\n\
         \n\
         schema-forge generate \"A CRM with contacts and companies\"\n",
    );

    Err(CliError::Config {
        message: "AI provider not configured. See 'schema-forge generate --help'.".into(),
    })
}
