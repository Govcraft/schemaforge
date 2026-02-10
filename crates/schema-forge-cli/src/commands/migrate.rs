use console::Term;
use schema_forge_backend::SchemaBackend;
use schema_forge_core::migration::DiffEngine;
use schema_forge_surrealdb::SurrealBackend;

use crate::cli::{GlobalOpts, MigrateArgs};
use crate::commands::parse::parse_all_schemas;
use crate::config::{load_config, resolve_db_params};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};
use crate::progress;

/// Run the `migrate` command: plan and optionally execute schema migrations.
pub async fn run(
    args: MigrateArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas(&args.paths)?;

    let config = load_config(global.config.as_deref())?;
    let db_params = resolve_db_params(&config, global);

    let spinner = if output.show_progress() {
        Some(progress::create_spinner("Connecting to backend..."))
    } else {
        None
    };

    let backend = SurrealBackend::connect_memory(&db_params.namespace, &db_params.database)
        .await
        .map_err(|e| {
            if let Some(sp) = &spinner {
                progress::finish_spinner_error(sp, "connection failed");
            }
            CliError::Backend(e)
        })?;

    if let Some(sp) = &spinner {
        progress::finish_spinner(sp, "Connected.");
    }

    let mut plans = Vec::new();
    let mut total_steps = 0usize;
    let mut schemas_affected = 0usize;

    for schema in &schemas {
        // Filter by --schema if specified
        if let Some(ref filter) = args.schema {
            if schema.name.as_str() != filter {
                continue;
            }
        }

        let existing = backend.load_schema_metadata(&schema.name).await?;
        let plan = if let Some(old) = existing {
            DiffEngine::diff(&old, schema)
        } else {
            DiffEngine::create_new(schema)
        };

        if plan.is_empty() {
            if output.mode == OutputMode::Human {
                output.status(&format!("{} (no changes)", schema.name.as_str()));
            }
        } else {
            total_steps += plan.steps.len();
            schemas_affected += 1;
        }

        plans.push((schema, plan));
    }

    // Render plan
    match output.mode {
        OutputMode::Human => {
            println!("Migration plan for {} schemas:", plans.len());
            println!();
            for (schema, plan) in &plans {
                if plan.is_empty() {
                    continue;
                }
                println!(
                    "{} ({} steps, {})",
                    schema.name.as_str(),
                    plan.steps.len(),
                    plan.overall_safety()
                );
                for (i, step) in plan.steps.iter().enumerate() {
                    println!("  {}. {} [{}]", i + 1, step, step.safety());
                }
                println!();
            }
            println!("Total: {total_steps} steps across {schemas_affected} schemas.");
            if !args.execute {
                println!("To apply: schema-forge migrate --execute");
            }
        }
        OutputMode::Json => {
            let json_plans: Vec<serde_json::Value> = plans
                .iter()
                .filter(|(_, p)| !p.is_empty())
                .map(|(schema, plan)| {
                    let steps: Vec<serde_json::Value> = plan
                        .steps
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "description": s.to_string(),
                                "safety": s.safety().to_string(),
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "schema": schema.name.as_str(),
                        "safety": plan.overall_safety().to_string(),
                        "steps": steps,
                    })
                })
                .collect();
            let json = serde_json::json!({
                "plans": json_plans,
                "total_steps": total_steps,
                "schemas_affected": schemas_affected,
            });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            for (schema, plan) in &plans {
                if plan.is_empty() {
                    continue;
                }
                for step in &plan.steps {
                    println!("{}\t{}\t{}", schema.name.as_str(), step, step.safety());
                }
            }
        }
    }

    // Execute if requested
    if args.execute {
        for (schema, plan) in &plans {
            if plan.is_empty() {
                continue;
            }

            // Safety check for destructive changes
            if plan.has_destructive_steps() && !args.force {
                let is_tty = Term::stderr().is_term();
                if !is_tty {
                    return Err(CliError::RequiresForce);
                }

                let confirm = dialoguer::Confirm::new()
                    .with_prompt(format!(
                        "Apply destructive migration to {}?",
                        schema.name.as_str()
                    ))
                    .default(false)
                    .interact()
                    .map_err(|_| CliError::Cancelled)?;

                if !confirm {
                    output.status(&format!("  Skipped {}", schema.name.as_str()));
                    continue;
                }
            }

            backend.apply_migration(&schema.name, &plan.steps).await?;
            backend.store_schema_metadata(schema).await?;
        }

        output.success(&format!(
            "Executed {total_steps} migration steps across {schemas_affected} schemas."
        ));
    }

    Ok(())
}
