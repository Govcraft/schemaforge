mod cli;
mod commands;
mod config;
#[allow(unused_assignments)]
mod diagnostic;
mod error;
mod output;
mod progress;

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();
    let output = output::OutputContext::from_global(&cli.global);

    let result = match cli.command {
        cli::Commands::Init(args) => commands::init::run(args, &cli.global, &output).await,
        cli::Commands::Parse(args) => commands::parse::run(args, &cli.global, &output).await,
        cli::Commands::Apply(args) => commands::apply::run(args, &cli.global, &output).await,
        cli::Commands::Migrate(args) => commands::migrate::run(args, &cli.global, &output).await,
        cli::Commands::Generate(args) => commands::generate::run(args, &cli.global, &output).await,
        cli::Commands::Serve(args) => commands::serve::run(args, &cli.global, &output).await,
        cli::Commands::Export { command } => {
            commands::export::run(command, &cli.global, &output).await
        }
        cli::Commands::Inspect(args) => commands::inspect::run(args, &cli.global, &output).await,
        cli::Commands::Policies { command } => {
            commands::policies::run(command, &cli.global, &output).await
        }
        #[cfg(feature = "cloud-ui")]
        cli::Commands::Templates { command } => {
            match command {
                cli::TemplateCommands::List => {
                    commands::templates::list(&output);
                    Ok(())
                }
                cli::TemplateCommands::Export(args) => {
                    commands::templates::export(&args, &output)
                }
            }
        }
        cli::Commands::Completions(args) => commands::completions::run(args),
    };

    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            output.print_error(&e);
            std::process::exit(e.exit_code() as i32);
        }
    }
}
