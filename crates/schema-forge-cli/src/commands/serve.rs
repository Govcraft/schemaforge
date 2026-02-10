use crate::cli::{GlobalOpts, ServeArgs};
use crate::error::CliError;
use crate::output::OutputContext;

/// Run the `serve` command (stub).
///
/// Full implementation depends on acton-service integration being configured.
pub async fn run(
    _args: ServeArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.warn(
        "The 'serve' command requires acton-service configuration.\n\
         \n\
         To start the SchemaForge development server:\n\
         \n\
         1. Ensure SurrealDB is running:\n\
         \n\
         surreal start memory\n\
         \n\
         2. Configure connection in config.toml:\n\
         \n\
         [database]\n\
         url = \"ws://localhost:8000\"\n\
         \n\
         3. Run again:\n\
         \n\
         schema-forge serve\n",
    );

    Err(CliError::Config {
        message: "Server configuration not available. See 'schema-forge serve --help'.".into(),
    })
}
