use console::Term;
use schema_forge_acton::DynSchemaBackend;
use schema_forge_core::{migration::MigrationSafety, types::SchemaDefinition};

use super::schema_update::SchemaUpdate;

use crate::cli::{ApplyArgs, GlobalOpts};
use crate::commands::parse::parse_all_schemas_with_global;
use crate::config::{load_svc_config, resolve_db_params};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `apply` command: parse schemas and apply to backend.
pub async fn run(
    args: ApplyArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.status("Parsing schemas...");
    let schemas = parse_all_schemas_with_global(&args.paths, global, output)?;
    output.status(&format!("  {} schemas parsed.", schemas.len()));

    let svc_config = load_svc_config(global)?;
    let db_params = resolve_db_params(&svc_config)?;

    let backend = super::connect_backend(&db_params, output).await?;

    apply_to_backend(&args, &schemas, backend.as_ref(), output).await
}

pub(super) async fn apply_to_backend(
    args: &ApplyArgs,
    schemas: &[SchemaDefinition],
    backend: &dyn DynSchemaBackend,
    output: &OutputContext,
) -> Result<(), CliError> {
    let mut total_steps = 0usize;
    let mut applied_schemas = 0usize;
    let mut metadata_only_updates = 0usize;

    for schema in schemas {
        let existing = backend.load_schema_metadata(&schema.name).await?;

        let update = SchemaUpdate::plan(existing.as_ref(), schema);
        let plan = &update.migration;
        if update.is_empty() {
            output.status(&format!("  {} .... no changes", schema.name.as_str()));
            continue;
        }

        // Safety check for destructive operations
        if plan.has_destructive_steps() && !args.force && !args.dry_run {
            let is_tty = Term::stderr().is_term();
            if !is_tty {
                return Err(CliError::RequiresForce);
            }

            // In human mode, show the plan and ask for confirmation
            output.warn(&format!(
                "{} migration includes destructive changes:",
                schema.name.as_str()
            ));
            for (i, step) in plan.steps.iter().enumerate() {
                let safety = step.safety();
                output.status(&format!("  {}. {} [{}]", i + 1, step, safety));
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

        // Render plan
        let safety_label = plan.overall_safety();
        match output.mode {
            OutputMode::Human => {
                if plan.is_empty() {
                    output.status(&format!(
                        "  {:<16} METADATA UPDATE (0 migration steps)",
                        schema.name.as_str()
                    ));
                } else if plan.steps.len() == 1
                    && matches!(
                        &plan.steps[0],
                        schema_forge_core::migration::MigrationStep::CreateSchema { .. }
                    )
                {
                    output.status(&format!(
                        "  {:<16} CREATE ({} fields){}",
                        schema.name.as_str(),
                        schema.fields.len(),
                        format_safety_tag(safety_label),
                    ));
                } else {
                    output.status(&format!(
                        "  {:<16} UPDATE ({} steps){}",
                        schema.name.as_str(),
                        plan.steps.len(),
                        format_safety_tag(safety_label),
                    ));
                }
            }
            OutputMode::Json | OutputMode::Plain => {
                // JSON summary is printed after all schemas
            }
        }

        if !args.dry_run {
            update.persist(backend).await?;
        }

        if plan.is_empty() {
            metadata_only_updates += 1;
        }
        total_steps += plan.steps.len();
        applied_schemas += 1;
    }

    // Generate policies if requested
    if args.with_policies && !args.dry_run {
        for schema in schemas {
            let policies = schema_forge_acton::cedar::generate_cedar_policies(schema);
            output.status(&format!(
                "  Generated {} Cedar policies for {}",
                policies.len(),
                schema.name.as_str()
            ));
        }
    }

    // Summary
    match output.mode {
        OutputMode::Human => {
            if args.dry_run {
                output.success(&format!(
                    "Dry run: {applied_schemas} schemas would be applied ({total_steps} migration steps)."
                ));
            } else {
                output.success(&format!(
                    "Applied {applied_schemas} schemas ({total_steps} migration steps)."
                ));
            }
        }
        OutputMode::Json => {
            let json = serde_json::json!({
                "dry_run": args.dry_run,
                "schemas_applied": applied_schemas,
                "total_steps": total_steps,
                "metadata_only_updates": metadata_only_updates,
            });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            println!("{applied_schemas}\t{total_steps}\t{}", args.dry_run);
        }
    }

    Ok(())
}

fn format_safety_tag(safety: MigrationSafety) -> String {
    match safety {
        MigrationSafety::Safe => "  [safe]".to_string(),
        MigrationSafety::RequiresConfirmation => "  [requires_confirmation]".to_string(),
        MigrationSafety::Destructive => "  [destructive]".to_string(),
        _ => String::new(),
    }
}
