use schema_forge_backend::SchemaBackend;
use schema_forge_core::types::SchemaDefinition;

use crate::cli::{GlobalOpts, InspectArgs};
use crate::config::{load_config, resolve_db_params};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `inspect` command: show registered schemas and their details.
pub async fn run(
    args: InspectArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let config = load_config(global.config.as_deref())?;
    let db_params = resolve_db_params(&config, global);

    let backend = super::connect_backend(&db_params, output).await?;

    let all_schemas = backend.list_schema_metadata().await?;

    if let Some(ref name) = args.schema {
        // Show specific schema
        let schema = all_schemas
            .iter()
            .find(|s| s.name.as_str() == name)
            .ok_or_else(|| CliError::SchemaNotFound { name: name.clone() })?;

        render_schema_detail(schema, output);
    } else {
        // Show all schemas
        render_schema_list(&all_schemas, output);
    }

    Ok(())
}

fn render_schema_list(schemas: &[SchemaDefinition], output: &OutputContext) {
    match output.mode {
        OutputMode::Human => {
            if schemas.is_empty() {
                output.status("No schemas registered.");
                return;
            }
            println!(
                "{:<20} {:<8} {:<8} {:<10} {:<8}",
                "Schema", "Version", "Fields", "Relations", "Indexed"
            );
            println!(
                "{:<20} {:<8} {:<8} {:<10} {:<8}",
                "------", "-------", "------", "---------", "-------"
            );
            for schema in schemas {
                let relations = schema
                    .fields
                    .iter()
                    .filter(|f| {
                        matches!(
                            f.field_type,
                            schema_forge_core::types::FieldType::Relation { .. }
                        )
                    })
                    .count();
                let indexed = schema.fields.iter().filter(|f| f.is_indexed()).count();
                println!(
                    "{:<20} {:<8} {:<8} {:<10} {:<8}",
                    schema.name.as_str(),
                    1, // version placeholder
                    schema.fields.len(),
                    relations,
                    indexed
                );
            }
        }
        OutputMode::Json => {
            let json_schemas: Vec<serde_json::Value> = schemas.iter().map(schema_to_json).collect();
            let json = serde_json::json!({ "schemas": json_schemas });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            for schema in schemas {
                let relations = schema
                    .fields
                    .iter()
                    .filter(|f| {
                        matches!(
                            f.field_type,
                            schema_forge_core::types::FieldType::Relation { .. }
                        )
                    })
                    .count();
                let indexed = schema.fields.iter().filter(|f| f.is_indexed()).count();
                println!(
                    "{}\t1\t{}\t{}\t{}",
                    schema.name.as_str(),
                    schema.fields.len(),
                    relations,
                    indexed
                );
            }
        }
    }
}

fn render_schema_detail(schema: &SchemaDefinition, output: &OutputContext) {
    match output.mode {
        OutputMode::Human => {
            println!("Schema: {} (version 1)", schema.name.as_str());
            println!();
            println!("Fields:");
            for field in &schema.fields {
                let modifiers: Vec<&str> =
                    field.modifiers.iter().map(|m| modifier_label(m)).collect();
                let mod_str = if modifiers.is_empty() {
                    String::new()
                } else {
                    format!("  {}", modifiers.join(" "))
                };
                println!(
                    "  {:<16} {:<24}{}",
                    field.name.as_str(),
                    field.field_type.to_string(),
                    mod_str
                );
            }
            if schema.annotations.is_empty() {
                println!();
                println!("Annotations: (none)");
            } else {
                println!();
                println!("Annotations:");
                for ann in &schema.annotations {
                    println!("  {ann}");
                }
            }
        }
        OutputMode::Json => {
            let json = schema_to_json(schema);
            output.print_json(&json);
        }
        OutputMode::Plain => {
            for field in &schema.fields {
                let modifiers: Vec<&str> =
                    field.modifiers.iter().map(|m| modifier_label(m)).collect();
                println!(
                    "{}\t{}\t{}",
                    field.name.as_str(),
                    field.field_type,
                    modifiers.join(",")
                );
            }
        }
    }
}

/// Map a field modifier to a human-readable label.
fn modifier_label(m: &schema_forge_core::types::FieldModifier) -> &'static str {
    match m {
        schema_forge_core::types::FieldModifier::Required => "required",
        schema_forge_core::types::FieldModifier::Indexed => "indexed",
        schema_forge_core::types::FieldModifier::Default { .. } => "default",
        _ => "unknown",
    }
}

fn schema_to_json(schema: &SchemaDefinition) -> serde_json::Value {
    let fields: Vec<serde_json::Value> = schema
        .fields
        .iter()
        .map(|f| {
            let modifiers: Vec<&str> = f.modifiers.iter().map(|m| modifier_label(m)).collect();
            serde_json::json!({
                "name": f.name.as_str(),
                "type": f.field_type.to_string(),
                "modifiers": modifiers,
            })
        })
        .collect();

    serde_json::json!({
        "name": schema.name.as_str(),
        "version": 1,
        "fields": fields,
    })
}
