use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs};
use crate::error::CliError;

/// Generate shell completion scripts and write to stdout.
pub fn run(args: CompletionsArgs) -> Result<(), CliError> {
    let shell = match args.shell.as_str() {
        "bash" => clap_complete::Shell::Bash,
        "zsh" => clap_complete::Shell::Zsh,
        "fish" => clap_complete::Shell::Fish,
        "powershell" => clap_complete::Shell::PowerShell,
        "elvish" => clap_complete::Shell::Elvish,
        other => {
            return Err(CliError::Other(format!("unsupported shell: {other}")));
        }
    };

    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "schema-forge",
        &mut std::io::stdout(),
    );

    Ok(())
}
