use console::Term;
use schema_forge_acton::DynSchemaBackend;
use schema_forge_core::types::SchemaDefinition;

use super::schema_update::SchemaUpdate;

use crate::cli::{GlobalOpts, MigrateArgs};
use crate::commands::parse::parse_all_schemas_with_global;
use crate::config::{load_svc_config, resolve_db_params};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `migrate` command: plan and optionally execute schema migrations.
pub async fn run(
    args: MigrateArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas_with_global(&args.paths, global, output)?;

    let svc_config = load_svc_config(global)?;
    let db_params = resolve_db_params(&svc_config)?;

    let backend = super::connect_backend(&db_params, output).await?;

    migrate_on_backend(&args, &schemas, backend.as_ref(), output).await
}

pub(super) async fn migrate_on_backend(
    args: &MigrateArgs,
    schemas: &[SchemaDefinition],
    backend: &dyn DynSchemaBackend,
    output: &OutputContext,
) -> Result<(), CliError> {
    let mut plans = Vec::new();
    let mut total_steps = 0usize;
    let mut schemas_affected = 0usize;

    for schema in schemas {
        // Filter by --schema if specified
        if let Some(ref filter) = args.schema {
            if schema.name.as_str() != filter {
                continue;
            }
        }

        let existing = backend.load_schema_metadata(&schema.name).await?;
        let update = SchemaUpdate::plan(existing.as_ref(), schema);
        let plan = &update.migration;

        if update.is_empty() {
            if output.mode == OutputMode::Human {
                output.status(&format!("{} (no changes)", schema.name.as_str()));
            }
        } else {
            total_steps += plan.steps.len();
            schemas_affected += 1;
        }

        plans.push(update);
    }

    // Render plan
    match output.mode {
        OutputMode::Human => {
            println!("Migration plan for {} schemas:", plans.len());
            println!();
            for update in &plans {
                let schema = &update.schema;
                let plan = &update.migration;
                if update.is_empty() {
                    continue;
                }
                if plan.is_empty() {
                    println!(
                        "{} (metadata update, 0 migration steps)",
                        schema.name.as_str()
                    );
                    println!();
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
                .filter(|update| !update.is_empty())
                .map(|update| {
                    let schema = &update.schema;
                    let plan = &update.migration;
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
                        "metadata_changed": update.metadata_changed,
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
            for update in &plans {
                let schema = &update.schema;
                let plan = &update.migration;
                if update.is_empty() {
                    continue;
                }
                if plan.is_empty() {
                    println!("{}\tmetadata update\tsafe", schema.name.as_str());
                }
                for step in &plan.steps {
                    println!("{}\t{}\t{}", schema.name.as_str(), step, step.safety());
                }
            }
        }
    }

    // Execute if requested
    if args.execute {
        for update in &plans {
            let schema = &update.schema;
            let plan = &update.migration;
            if update.is_empty() {
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

            update.persist(backend).await?;
        }

        output.success(&format!(
            "Executed {total_steps} migration steps across {schemas_affected} schemas."
        ));
    }

    Ok(())
}
