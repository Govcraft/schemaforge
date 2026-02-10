use crate::cli::{ExportCommands, ExportOpenapiArgs, GlobalOpts};
use crate::commands::parse::parse_all_schemas;
use crate::error::CliError;
use crate::output::OutputContext;

/// Run the `export` subcommand.
pub async fn run(
    command: ExportCommands,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    match command {
        ExportCommands::Openapi(args) => run_openapi(args, global, output).await,
    }
}

async fn run_openapi(
    args: ExportOpenapiArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas(&args.paths)?;

    // Build a basic OpenAPI spec from schema definitions
    let mut paths = serde_json::Map::new();
    let mut components_schemas = serde_json::Map::new();

    for schema in &schemas {
        let name = schema.name.as_str();
        let lower = name.to_ascii_lowercase();

        // Build schema component
        let mut properties = serde_json::Map::new();
        let mut required_fields = Vec::new();

        for field in &schema.fields {
            let field_name = field.name.as_str();
            let field_schema = serde_json::json!({
                "type": openapi_type_for(&field.field_type),
            });
            properties.insert(field_name.to_string(), field_schema);

            if field.is_required() {
                required_fields.push(serde_json::Value::String(field_name.to_string()));
            }
        }

        let mut component = serde_json::json!({
            "type": "object",
            "properties": properties,
        });
        if !required_fields.is_empty() {
            component["required"] = serde_json::Value::Array(required_fields);
        }
        components_schemas.insert(name.to_string(), component);

        // Build path entries
        let collection_path = format!("{}/schemas/{lower}/entities", args.base_path);
        let item_path = format!("{}/schemas/{lower}/entities/{{id}}", args.base_path);

        paths.insert(
            collection_path,
            serde_json::json!({
                "get": {
                    "summary": format!("List {name} entities"),
                    "responses": {
                        "200": {
                            "description": format!("List of {name} entities"),
                        }
                    }
                },
                "post": {
                    "summary": format!("Create a {name} entity"),
                    "responses": {
                        "201": {
                            "description": format!("Created {name} entity"),
                        }
                    }
                }
            }),
        );

        paths.insert(
            item_path,
            serde_json::json!({
                "get": {
                    "summary": format!("Get a {name} entity by ID"),
                    "responses": {
                        "200": {
                            "description": format!("{name} entity"),
                        }
                    }
                },
                "put": {
                    "summary": format!("Update a {name} entity"),
                    "responses": {
                        "200": {
                            "description": format!("Updated {name} entity"),
                        }
                    }
                },
                "delete": {
                    "summary": format!("Delete a {name} entity"),
                    "responses": {
                        "204": {
                            "description": "Entity deleted",
                        }
                    }
                }
            }),
        );
    }

    let openapi_spec = serde_json::json!({
        "openapi": args.spec_version,
        "info": {
            "title": "SchemaForge API",
            "version": "0.1.0",
            "description": "Auto-generated API from SchemaForge schema definitions",
        },
        "paths": paths,
        "components": {
            "schemas": components_schemas,
        }
    });

    if let Some(output_path) = &args.output {
        let json_str = serde_json::to_string_pretty(&openapi_spec)
            .map_err(|e| CliError::Other(format!("failed to serialize OpenAPI spec: {e}")))?;
        std::fs::write(output_path, json_str).map_err(|e| CliError::Io {
            path: output_path.clone(),
            source: e,
        })?;
        output.success(&format!("Wrote OpenAPI spec to {}", output_path.display()));
    } else {
        output.print_json(&openapi_spec);
    }

    Ok(())
}

fn openapi_type_for(field_type: &schema_forge_core::types::FieldType) -> &'static str {
    use schema_forge_core::types::FieldType;
    match field_type {
        FieldType::Text(_) => "string",
        FieldType::Integer(_) => "integer",
        FieldType::Float(_) => "number",
        FieldType::Boolean => "boolean",
        FieldType::Enum(_) => "string",
        FieldType::Array(_) => "array",
        FieldType::Composite(_) => "object",
        FieldType::Relation { .. } => "string",
        _ => "string",
    }
}
