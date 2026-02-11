use std::sync::Arc;

use schema_forge_acton::SchemaRegistry;
use schema_forge_ai::SchemaForgeAgent;
use schema_forge_surrealdb::SurrealBackend;

use crate::cli::{GenerateArgs, GlobalOpts};
use crate::error::CliError;
use crate::output::OutputContext;

/// Run the `generate` command: AI-powered schema generation.
///
/// Supports four provider modes:
/// - `auto`: reads acton-ai.toml via `SchemaForgeAgent::from_config()`
/// - `ollama`: local Ollama with optional model override
/// - `anthropic`: Anthropic API (requires ANTHROPIC_API_KEY env)
/// - `openai`: OpenAI API (requires OPENAI_API_KEY env)
///
/// With a description: single-shot generation, writes to file or stdout.
/// Without a description: enters interactive CLI mode.
pub async fn run(
    args: GenerateArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    // 1. Early validation: --batch requires a description
    if args.batch && args.description.is_none() {
        return Err(CliError::Config {
            message: "--batch requires a description argument. \
                      Example: schema-forge generate \"A CRM system\" --batch"
                .into(),
        });
    }

    // 2. Create in-memory backend (generate does not need persistent storage)
    let backend = SurrealBackend::connect_memory("schemaforge", "generate")
        .await
        .map_err(|e| CliError::Ai {
            message: format!("failed to create in-memory backend: {e}"),
        })?;
    let backend = Arc::new(backend);
    let registry = SchemaRegistry::new();

    // 3. Build the agent based on provider
    let agent = build_agent(&args, registry, backend.clone(), output).await?;

    // 4. Dispatch: single-shot with description, or interactive
    match &args.description {
        Some(description) => {
            // Single-shot generation with reliable DSL extraction
            output.status("Generating schema...");
            let result = agent
                .generate_dsl(description)
                .await
                .map_err(|e| CliError::Ai {
                    message: e.to_string(),
                })?;

            output.status(&format!(
                "Extracted {} schema(s) via {} source",
                result.schema_count, result.source,
            ));

            // Write output
            if let Some(ref path) = args.output {
                std::fs::write(path, &result.dsl).map_err(|e| CliError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                output.success(&format!("Schema written to {}", path.display()));
            } else {
                println!("{}", result.dsl);
            }
        }
        None => {
            // Interactive mode
            schema_forge_ai::run_interactive_cli(&agent)
                .await
                .map_err(|e| CliError::Ai {
                    message: e.to_string(),
                })?;
        }
    }

    Ok(())
}

/// Build a `SchemaForgeAgent` based on the chosen provider.
async fn build_agent(
    args: &GenerateArgs,
    registry: SchemaRegistry,
    backend: Arc<dyn schema_forge_acton::DynForgeBackend>,
    output: &OutputContext,
) -> Result<SchemaForgeAgent, CliError> {
    match args.provider.as_str() {
        "auto" => {
            output.status("Loading AI provider from acton-ai.toml...");
            SchemaForgeAgent::from_config(registry, backend)
                .await
                .map_err(|e| CliError::Ai {
                    message: format!("failed to load AI config: {e}"),
                })
        }
        "ollama" => {
            let model = args
                .model
                .clone()
                .unwrap_or_else(|| "llama3.1:8b".to_string());
            output.status(&format!("Using Ollama provider with model '{model}'..."));
            SchemaForgeAgent::ollama(&model, registry, backend)
                .await
                .map_err(|e| CliError::Ai {
                    message: format!("failed to initialize Ollama: {e}"),
                })
        }
        "anthropic" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| CliError::Config {
                message: "ANTHROPIC_API_KEY environment variable is required \
                          for the 'anthropic' provider."
                    .into(),
            })?;
            output.status("Using Anthropic provider...");
            SchemaForgeAgent::anthropic(api_key, registry, backend)
                .await
                .map_err(|e| CliError::Ai {
                    message: format!("failed to initialize Anthropic: {e}"),
                })
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| CliError::Config {
                message: "OPENAI_API_KEY environment variable is required \
                          for the 'openai' provider."
                    .into(),
            })?;
            output.status("Using OpenAI provider...");
            SchemaForgeAgent::openai(api_key, registry, backend)
                .await
                .map_err(|e| CliError::Ai {
                    message: format!("failed to initialize OpenAI: {e}"),
                })
        }
        other => Err(CliError::Config {
            message: format!(
                "unknown provider '{other}'. Valid providers: auto, ollama, anthropic, openai"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_provider_returns_config_error() {
        let args = GenerateArgs {
            description: Some("test".into()),
            output: None,
            provider: "unknown-provider".into(),
            model: None,
            batch: false,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let backend = SurrealBackend::connect_memory("test", "gen_test")
                .await
                .unwrap();
            let backend = Arc::new(backend);
            let registry = SchemaRegistry::new();
            let output = crate::output::OutputContext {
                mode: crate::output::OutputMode::Plain,
                verbose: 0,
                quiet: true,
                use_color: false,
            };
            build_agent(&args, registry, backend, &output).await
        });

        let err = result.err().expect("expected error for unknown provider");
        assert!(matches!(err, CliError::Config { .. }));
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn batch_without_description_returns_config_error() {
        let args = GenerateArgs {
            description: None,
            output: None,
            provider: "auto".into(),
            model: None,
            batch: true,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let global = crate::cli::GlobalOpts {
                config: None,
                format: "human".into(),
                verbose: 0,
                quiet: true,
                no_color: true,
                db_url: None,
                db_ns: None,
                db_name: None,
            };
            let output = crate::output::OutputContext {
                mode: crate::output::OutputMode::Plain,
                verbose: 0,
                quiet: true,
                use_color: false,
            };
            run(args, &global, &output).await
        });

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, CliError::Config { .. }));
        assert!(err.to_string().contains("--batch requires a description"));
    }

    #[test]
    fn anthropic_without_env_returns_config_error() {
        // Ensure env var is not set
        std::env::remove_var("ANTHROPIC_API_KEY");

        let args = GenerateArgs {
            description: Some("test".into()),
            output: None,
            provider: "anthropic".into(),
            model: None,
            batch: false,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let backend = SurrealBackend::connect_memory("test", "gen_test2")
                .await
                .unwrap();
            let backend = Arc::new(backend);
            let registry = SchemaRegistry::new();
            let output = crate::output::OutputContext {
                mode: crate::output::OutputMode::Plain,
                verbose: 0,
                quiet: true,
                use_color: false,
            };
            build_agent(&args, registry, backend, &output).await
        });

        let err = result
            .err()
            .expect("expected error for missing ANTHROPIC_API_KEY");
        assert!(matches!(err, CliError::Config { .. }));
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn openai_without_env_returns_config_error() {
        // Ensure env var is not set
        std::env::remove_var("OPENAI_API_KEY");

        let args = GenerateArgs {
            description: Some("test".into()),
            output: None,
            provider: "openai".into(),
            model: None,
            batch: false,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let backend = SurrealBackend::connect_memory("test", "gen_test3")
                .await
                .unwrap();
            let backend = Arc::new(backend);
            let registry = SchemaRegistry::new();
            let output = crate::output::OutputContext {
                mode: crate::output::OutputMode::Plain,
                verbose: 0,
                quiet: true,
                use_color: false,
            };
            build_agent(&args, registry, backend, &output).await
        });

        let err = result
            .err()
            .expect("expected error for missing OPENAI_API_KEY");
        assert!(matches!(err, CliError::Config { .. }));
        assert!(err.to_string().contains("OPENAI_API_KEY"));
    }

    #[test]
    fn generate_args_defaults() {
        // Verify the CLI arg types are what we expect
        let args = GenerateArgs {
            description: None,
            output: None,
            provider: "auto".into(),
            model: None,
            batch: false,
        };
        assert_eq!(args.provider, "auto");
        assert!(args.description.is_none());
        assert!(!args.batch);
    }
}
