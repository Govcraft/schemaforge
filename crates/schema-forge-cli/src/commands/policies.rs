use std::fs;

use schema_forge_acton::cedar::generate_cedar_policies;

use crate::cli::{GlobalOpts, PolicyCommands, PolicyListArgs, PolicyRegenerateArgs};
use crate::commands::parse::parse_all_schemas;
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `policies` subcommand.
pub async fn run(
    command: PolicyCommands,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    match command {
        PolicyCommands::List(args) => run_list(args, global, output).await,
        PolicyCommands::Regenerate(args) => run_regenerate(args, global, output).await,
    }
}

async fn run_list(
    args: PolicyListArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    // For list, we need schemas. Parse from default location.
    let schemas = parse_all_schemas(&[std::path::PathBuf::from("schemas/")])?;

    for schema in &schemas {
        if let Some(ref filter) = args.schema {
            if schema.name.as_str() != filter {
                continue;
            }
        }

        let policies = generate_cedar_policies(schema);

        match output.mode {
            OutputMode::Human => {
                println!("Cedar policies for {}:", schema.name.as_str());
                println!();
                for (i, policy) in policies.iter().enumerate() {
                    println!("  {}. {}", i + 1, policy.description);
                }
                println!();
            }
            OutputMode::Json => {
                let json_policies: Vec<serde_json::Value> = policies
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "description": p.description,
                            "cedar_text": p.cedar_text,
                        })
                    })
                    .collect();
                let json = serde_json::json!({
                    "schema": schema.name.as_str(),
                    "policies": json_policies,
                });
                output.print_json(&json);
            }
            OutputMode::Plain => {
                for policy in &policies {
                    println!("{}\t{}", schema.name.as_str(), policy.description);
                }
            }
        }
    }

    if let Some(filter) = &args.schema {
        if !schemas.iter().any(|s| s.name.as_str() == filter) {
            return Err(CliError::SchemaNotFound {
                name: filter.clone(),
            });
        }
    }

    output.success("Use 'schema-forge policies regenerate' to write policy files.");
    Ok(())
}

async fn run_regenerate(
    args: PolicyRegenerateArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas(&[std::path::PathBuf::from("schemas/")])?;

    // Create output directory
    fs::create_dir_all(&args.output_dir).map_err(|e| CliError::Io {
        path: args.output_dir.clone(),
        source: e,
    })?;

    let mut total_files = 0usize;

    for schema in &schemas {
        if let Some(ref filter) = args.schema {
            if schema.name.as_str() != filter {
                continue;
            }
        }

        let policies = generate_cedar_policies(schema);
        let filename = format!("{}.cedar", schema.name.as_str().to_ascii_lowercase());
        let filepath = args.output_dir.join(&filename);

        if filepath.exists() && !args.force {
            output.warn(&format!(
                "Skipping {} (exists, use --force to overwrite)",
                filepath.display()
            ));
            continue;
        }

        let content: String = policies
            .iter()
            .map(|p| format!("// {}\n{}\n", p.description, p.cedar_text))
            .collect::<Vec<_>>()
            .join("\n");

        fs::write(&filepath, content).map_err(|e| CliError::Io {
            path: filepath.clone(),
            source: e,
        })?;

        total_files += 1;
        output.status(&format!("  Wrote {}", filepath.display()));
    }

    output.success(&format!(
        "Regenerated {total_files} policy files in {}",
        args.output_dir.display()
    ));

    Ok(())
}
