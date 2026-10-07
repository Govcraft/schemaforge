use crate::cli::{ExportCommands, ExportOpenapiArgs, GlobalOpts};
use crate::commands::parse::parse_all_schemas_with_global;
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
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas_with_global(&args.paths, global, output)?;

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

    #[cfg(feature = "sse")]
    paths.insert(format!("{}/schemas/{{schema}}/events", args.base_path), serde_json::json!({"get": {
        "summary": "Subscribe to authorized committed entity changes",
        "description": "Opt-in authenticated stream. Equality filters use known readable schema fields. No replay; refetch on reconnect. Last-Event-ID is ignored.",
        "security": [{"bearerAuth": []}],
        "parameters": [{"name": "schema", "in": "path", "required": true, "schema": {"type": "string"}}, {"name": "X-Active-Tenant", "in": "header", "schema": {"type": "string"}}],
        "responses": {"200": {"description": "entity.created, entity.updated, entity.deleted; closed requires reconnect/refetch", "content": {"text/event-stream": {"schema": {"type": "string"}}}}, "400": {"description": "Invalid equality filter or active tenant"}, "401": {"description": "Bearer authentication required"}, "403": {"description": "Read or tenant membership denied"}, "404": {"description": "Events disabled or schema unknown"}, "429": {"description": "Connection limit reached"}}
    }}));
    add_login_schema_and_path(&mut paths, &mut components_schemas, &args.base_path);
    #[cfg(feature = "oauth")]
    add_oauth_paths(&mut paths, &args.base_path);

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
            "securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer", "bearerFormat": "PASETO"}},
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

fn add_login_schema_and_path(
    paths: &mut serde_json::Map<String, serde_json::Value>,
    components: &mut serde_json::Map<String, serde_json::Value>,
    base: &str,
) {
    components.insert("LoginResponse".into(), serde_json::json!({
        "type": "object", "required": ["token", "expires_at", "roles"],
        "properties": {"token": {"type": "string"}, "expires_at": {"type": "string", "format": "date-time"}, "roles": {"type": "array", "items": {"type": "string"}}}
    }));
    paths.insert(format!("{base}/auth/login"), serde_json::json!({"post": {
        "summary": "Obtain a password session", "security": [],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "required": ["username", "password"], "properties": {"username": {"type": "string"}, "password": {"type": "string", "format": "password"}}}}}},
        "responses": {"200": {"description": "PASETO session", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/LoginResponse"}}}}, "401": {"description": "Invalid credentials"}, "404": {"description": "Password login disabled"}}
    }}));
    paths.insert(format!("{base}/auth/revocations"), serde_json::json!({"post": {
        "summary": "Revoke older bearer tokens for a subject (platform_admin only)",
        "requestBody": {"required": true, "content": {"application/json": {"schema": {
            "type": "object", "additionalProperties": false, "required": ["subject", "not_before"],
            "properties": {"subject": {"type": "string", "minLength": 1, "maxLength": 512},
                "not_before": {"type": "integer", "format": "int64", "minimum": 0,
                    "description": "Inclusive Unix-second issuance cutoff"}}
        }}}},
        "responses": {"200": {"description": "Persisted subject and effective monotonic cutoff"},
            "401": {"description": "Authentication required or token revoked"},
            "403": {"description": "platform_admin required"},
            "422": {"description": "Invalid subject or cutoff"},
            "502": {"description": "Revocation storage unavailable"}}
    }}));
}

#[cfg(feature = "oauth")]
fn add_oauth_paths(paths: &mut serde_json::Map<String, serde_json::Value>, base: &str) {
    paths.insert(format!("{base}/auth/oauth/providers"), serde_json::json!({"get": {
        "summary": "List configured OAuth providers", "security": [], "responses": {"200": {"description": "Configured names", "content": {"application/json": {"schema": {"type": "array", "items": {"type": "string"}}}}}, "404": {"description": "OAuth disabled"}}
    }}));
    for (route, summary, parameters) in [
        (
            "start",
            "Redirect to provider authorization",
            serde_json::json!([
                {"name": "provider", "in": "path", "required": true, "schema": {"type": "string"}},
                {"name": "return_to", "in": "query", "required": true, "schema": {"type": "string", "format": "uri"}},
                {"name": "invite_id", "in": "query", "schema": {"type": "string"}}
            ]),
        ),
        (
            "callback",
            "Consume OAuth state and redirect with a login code",
            serde_json::json!([
                {"name": "provider", "in": "path", "required": true, "schema": {"type": "string"}},
                {"name": "code", "in": "query", "required": true, "schema": {"type": "string"}},
                {"name": "state", "in": "query", "required": true, "schema": {"type": "string"}}
            ]),
        ),
    ] {
        paths.insert(format!("{base}/auth/oauth/{{provider}}/{route}"), serde_json::json!({"get": {
            "summary": summary, "security": [], "parameters": parameters,
            "responses": {"302": {"description": "Redirect with no bearer token in its URL", "headers": {"Location": {"schema": {"type": "string", "format": "uri"}}}}, "400": {"description": "Invalid return target"}, "401": {"description": "Invalid provider identity, state, or invitation"}, "403": {"description": "Invitation required"}, "404": {"description": "Unknown provider or OAuth disabled"}, "409": {"description": "Account or identity already exists"}}
        }}));
    }
    paths.insert(format!("{base}/auth/oauth/exchange"), serde_json::json!({"post": {
        "summary": "Exchange a single-use login code valid for 60 seconds", "security": [],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "required": ["code"], "properties": {"code": {"type": "string"}}}}}},
        "responses": {"200": {"description": "PASETO session", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/LoginResponse"}}}}, "401": {"description": "Invalid, expired, or consumed code"}, "404": {"description": "OAuth disabled"}}
    }}));
}
