//! `schema-forge hooks` subcommand: generate, list, and diff scaffolds for
//! `@hook(...)` annotations declared in your schemas.
//!
//! The generator emits a self-contained `acton-service` gRPC project rooted
//! at `--out-dir`. Layout:
//!
//! ```text
//! <out-dir>/
//!   Cargo.toml
//!   build.rs
//!   proto/
//!     <schema>_hooks.proto         # one per annotated schema
//!   src/
//!     main.rs                       # assembles all generated services
//!     hooks/
//!       mod.rs                      # re-exports per-schema modules
//!       <schema>.rs                 # per-schema service impl + stubs
//!       <schema>/
//!         <event>.prompt.md         # AI prompt file per stub (Phase 4)
//! ```
//!
//! Re-running the generator does NOT clobber existing `<schema>.rs`
//! implementation files unless `--force` is passed — only proto files,
//! `main.rs`, and prompt files are rewritten.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use heck::{ToPascalCase, ToSnakeCase};
use schema_forge_core::types::{Annotation, FieldType, HookEvent, SchemaDefinition};

use crate::cli::{GlobalOpts, HooksCommands, HooksDiffArgs, HooksGenerateArgs, HooksListArgs};
use crate::commands::parse::parse_all_schemas;
use crate::error::CliError;
use crate::output::OutputContext;

/// Top-level dispatch for `schema-forge hooks ...`.
pub async fn run(
    command: HooksCommands,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    match command {
        HooksCommands::Generate(args) => generate(args, global, output),
        HooksCommands::List(args) => list(args, global, output),
        HooksCommands::Diff(args) => diff(args, global, output),
    }
}

// ---------------------------------------------------------------------------
// hooks generate
// ---------------------------------------------------------------------------

/// Pulled-out view of one schema's hook annotations, ready for codegen.
struct SchemaHooks {
    name: String,
    pascal: String,
    snake: String,
    proto_package: String,
    schema: SchemaDefinition,
    events: Vec<(HookEvent, String)>,
}

impl SchemaHooks {
    fn from(def: SchemaDefinition) -> Option<Self> {
        let name = def.name.as_str().to_string();
        let pascal = name.to_pascal_case();
        let snake = name.to_snake_case();
        let proto_package = format!("schema_forge_hooks.{snake}");
        let mut events: Vec<(HookEvent, String)> = def
            .annotations
            .iter()
            .filter_map(|a| match a {
                Annotation::Hook { event, intent } => Some((*event, intent.clone())),
                _ => None,
            })
            .collect();
        if events.is_empty() {
            return None;
        }
        events.sort_by_key(|(e, _)| *e);
        Some(Self {
            name,
            pascal,
            snake,
            proto_package,
            schema: def,
            events,
        })
    }
}

fn generate(
    args: HooksGenerateArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.status(&format!(
        "Scanning schemas in {}...",
        args.schema_dir.display()
    ));
    let schemas = parse_all_schemas(std::slice::from_ref(&args.schema_dir))?;
    let mut hooked: Vec<SchemaHooks> = schemas
        .into_iter()
        .filter_map(SchemaHooks::from)
        .collect();

    if let Some(only) = &args.schema {
        hooked.retain(|h| h.name == *only);
        if hooked.is_empty() {
            return Err(CliError::Config {
                message: format!("schema '{only}' has no @hook(...) annotations"),
            });
        }
    } else if !args.all {
        return Err(CliError::Config {
            message: "specify --all or --schema <name>".to_string(),
        });
    }

    if hooked.is_empty() {
        output.warn("no schemas with @hook(...) annotations found");
        return Ok(());
    }

    output.status(&format!(
        "  found {} schema(s) with hooks",
        hooked.len()
    ));

    write_project(&args.out_dir, &hooked, args.force)?;

    output.success(&format!(
        "Hook service scaffold written to {}",
        args.out_dir.display()
    ));
    output.status("  Next steps:");
    output.status(&format!(
        "    cd {} && cargo check",
        args.out_dir.display()
    ));
    output.status("    Implement each TODO in src/hooks/<schema>.rs");
    output.status("    Read the .prompt.md files for AI-assist prompts");

    Ok(())
}

fn write_project(
    out_dir: &Path,
    hooked: &[SchemaHooks],
    force: bool,
) -> Result<(), CliError> {
    fs::create_dir_all(out_dir).map_err(io_err)?;
    fs::create_dir_all(out_dir.join("proto")).map_err(io_err)?;
    fs::create_dir_all(out_dir.join("src")).map_err(io_err)?;
    fs::create_dir_all(out_dir.join("src/hooks")).map_err(io_err)?;

    let project_name = out_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("hooks-service")
        .to_snake_case();

    write_file(&out_dir.join("Cargo.toml"), &render_cargo_toml(&project_name), true)?;
    write_file(&out_dir.join("build.rs"), BUILD_RS, true)?;
    write_file(&out_dir.join("src/main.rs"), &render_main_rs(hooked), true)?;
    write_file(&out_dir.join("src/hooks/mod.rs"), &render_hooks_mod(hooked), true)?;

    for h in hooked {
        let proto_path = out_dir.join("proto").join(format!("{}_hooks.proto", h.snake));
        write_file(&proto_path, &render_proto(h), true)?;

        // The per-schema impl is preserved by default — only `--force`
        // overwrites it. The first generation always writes a fresh stub.
        let impl_path = out_dir.join("src/hooks").join(format!("{}.rs", h.snake));
        write_file(&impl_path, &render_impl_stub(h), force || !impl_path.exists())?;

        // Prompt files (Phase 4) are always rewritten — they describe the
        // schema as it stands today and may have been updated.
        let prompt_dir = out_dir.join("src/hooks").join(&h.snake);
        fs::create_dir_all(&prompt_dir).map_err(io_err)?;
        for (event, intent) in &h.events {
            let prompt_path =
                prompt_dir.join(format!("{}.prompt.md", event.as_str()));
            write_file(&prompt_path, &render_prompt(h, *event, intent), true)?;
        }
    }

    Ok(())
}

fn write_file(path: &Path, contents: &str, overwrite: bool) -> Result<(), CliError> {
    if path.exists() && !overwrite {
        return Ok(());
    }
    fs::write(path, contents).map_err(io_err)
}

fn io_err(e: std::io::Error) -> CliError {
    CliError::Config {
        message: format!("io error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

const BUILD_RS: &str = r#"use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let descriptor_path = out_dir.join("hooks_descriptor.bin");

    let proto_files: Vec<PathBuf> = std::fs::read_dir("proto")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("proto"))
        .collect();

    tonic_prost_build::configure()
        .file_descriptor_set_path(&descriptor_path)
        .build_server(true)
        .build_client(false)
        .out_dir(&out_dir)
        .compile_protos(&proto_files, &[PathBuf::from("proto")])?;

    for f in &proto_files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    println!(
        "cargo:rustc-env=HOOKS_DESCRIPTOR_PATH={}",
        descriptor_path.display()
    );
    Ok(())
}
"#;

fn render_cargo_toml(project_name: &str) -> String {
    format!(
        r#"[package]
name = "{project_name}"
version = "0.1.0"
edition = "2021"

[dependencies]
prost = "0.14"
tokio = {{ version = "1", features = ["full"] }}
tonic = "0.14"
tonic-prost = "0.14"
tracing = "0.1"
tracing-subscriber = "0.3"

[build-dependencies]
tonic-build = "0.14"
tonic-prost-build = "0.14"
"#
    )
}

fn render_main_rs(hooked: &[SchemaHooks]) -> String {
    let mut s = String::new();
    s.push_str("//! Generated by `schema-forge hooks generate`.\n");
    s.push_str("//!\n");
    s.push_str("//! Re-running the generator regenerates this file. Edit\n");
    s.push_str("//! `src/hooks/<schema>.rs` to implement per-event logic.\n\n");
    s.push_str("mod hooks;\n\n");
    s.push_str("mod pb {\n");
    for h in hooked {
        s.push_str(&format!("    pub mod {} {{\n", h.snake));
        s.push_str(&format!(
            "        tonic::include_proto!(\"{}\");\n",
            h.proto_package
        ));
        s.push_str("    }\n");
    }
    s.push_str("}\n\n");
    s.push_str("use tonic::transport::Server;\n\n");
    s.push_str("#[tokio::main]\n");
    s.push_str("async fn main() -> Result<(), Box<dyn std::error::Error>> {\n");
    s.push_str("    tracing_subscriber::fmt::init();\n");
    s.push_str("    let addr = \"0.0.0.0:9090\".parse()?;\n");
    s.push_str("    tracing::info!(\"hook service listening on {addr}\");\n\n");
    s.push_str("    Server::builder()\n");
    for h in hooked {
        s.push_str(&format!(
            "        .add_service(pb::{snake}::{snake}_hooks_server::{pascal}HooksServer::new(hooks::{snake}::Service::default()))\n",
            snake = h.snake,
            pascal = h.pascal,
        ));
    }
    s.push_str("        .serve(addr)\n");
    s.push_str("        .await?;\n");
    s.push_str("    Ok(())\n");
    s.push_str("}\n");
    s
}

fn render_hooks_mod(hooked: &[SchemaHooks]) -> String {
    let mut s = String::new();
    s.push_str("//! Per-schema hook service implementations.\n\n");
    for h in hooked {
        s.push_str(&format!("pub mod {};\n", h.snake));
    }
    s
}

fn render_proto(h: &SchemaHooks) -> String {
    let mut s = String::new();
    s.push_str("syntax = \"proto3\";\n\n");
    s.push_str(&format!("package {};\n\n", h.proto_package));
    s.push_str(&format!(
        "// Generated from schema `{}`. Re-run `schema-forge hooks generate`\n",
        h.name
    ));
    s.push_str("// to refresh after schema changes.\n\n");

    s.push_str(&format!("service {}Hooks {{\n", h.pascal));
    for (event, _) in &h.events {
        let method = event_to_method(*event);
        s.push_str(&format!(
            "  rpc {method}({pascal}{method}Request) returns ({pascal}{method}Response);\n",
            pascal = h.pascal,
        ));
    }
    s.push_str("}\n\n");

    let scalar_fields = scalar_proto_fields(&h.schema);

    for (event, _) in &h.events {
        let method = event_to_method(*event);
        // Request
        s.push_str(&format!("message {}{}Request {{\n", h.pascal, method));
        s.push_str("  string operation = 1;\n");
        s.push_str("  optional string user_id = 2;\n");
        s.push_str("  optional string entity_id = 3;\n");
        let mut tag = 100;
        for (name, ty, required) in &scalar_fields {
            if *required {
                s.push_str(&format!("  {ty} {name} = {tag};\n"));
            } else {
                s.push_str(&format!("  optional {ty} {name} = {tag};\n"));
            }
            tag += 1;
        }
        s.push_str("}\n\n");

        // Response — every field is optional (modifiable) plus abort_reason
        s.push_str(&format!("message {}{}Response {{\n", h.pascal, method));
        s.push_str("  optional string abort_reason = 1;\n");
        let mut tag = 100;
        for (name, ty, _) in &scalar_fields {
            s.push_str(&format!("  optional {ty} {name} = {tag};\n"));
            tag += 1;
        }
        s.push_str("}\n\n");
    }

    s
}

/// Map a schema's field definitions to (name, proto-type, required) triples.
fn scalar_proto_fields(schema: &SchemaDefinition) -> Vec<(String, &'static str, bool)> {
    use schema_forge_core::types::FieldModifier;
    schema
        .fields
        .iter()
        .map(|f| {
            let ty = match &f.field_type {
                FieldType::Text(_) => "string",
                FieldType::Integer(_) => "int64",
                FieldType::Float(_) => "double",
                FieldType::Boolean => "bool",
                FieldType::DateTime => "string",
                FieldType::Enum(_) => "string",
                FieldType::Relation { .. } => "string",
                _ => "string",
            };
            let required = f.modifiers.iter().any(|m| matches!(m, FieldModifier::Required));
            (f.name.as_str().to_string(), ty, required)
        })
        .collect()
}

fn render_impl_stub(h: &SchemaHooks) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "//! Service impl for `{}` — generated stub.\n",
        h.name
    ));
    s.push_str("//!\n");
    s.push_str(
        "//! Re-running `schema-forge hooks generate` does NOT overwrite this\n",
    );
    s.push_str("//! file (use `--force` to opt in). Add real logic to each method.\n\n");

    s.push_str(&format!(
        "use crate::pb::{snake}::{snake}_hooks_server::{pascal}Hooks;\n",
        snake = h.snake,
        pascal = h.pascal,
    ));
    s.push_str(&format!("use crate::pb::{}::*;\n\n", h.snake));
    s.push_str("use tonic::{Request, Response, Status};\n\n");

    s.push_str("#[derive(Default)]\n");
    s.push_str("pub struct Service;\n\n");

    s.push_str("#[tonic::async_trait]\n");
    s.push_str(&format!("impl {}Hooks for Service {{\n", h.pascal));
    for (event, intent) in &h.events {
        let method = event_to_method(*event);
        let method_snake = event.as_str();
        s.push_str(&format!("    /// {intent}\n"));
        s.push_str(&format!(
            "    async fn {method_snake}(&self, request: Request<{pascal}{method}Request>) -> Result<Response<{pascal}{method}Response>, Status> {{\n",
            pascal = h.pascal,
        ));
        s.push_str("        let _req = request.into_inner();\n");
        s.push_str(&format!(
            "        // TODO: implement {method_snake} for `{schema}` — see\n",
            schema = h.name
        ));
        s.push_str(&format!(
            "        //       src/hooks/{}/{}.prompt.md\n",
            h.snake, method_snake
        ));
        s.push_str(&format!(
            "        Ok(Response::new({pascal}{method}Response::default()))\n",
            pascal = h.pascal,
        ));
        s.push_str("    }\n");
    }
    s.push_str("}\n");
    s
}

fn render_prompt(h: &SchemaHooks, event: HookEvent, intent: &str) -> String {
    let method_snake = event.as_str();
    let scalar_fields = scalar_proto_fields(&h.schema);
    let mut s = String::new();
    s.push_str(&format!(
        "# `{}` — `{}`\n\n",
        h.name, method_snake
    ));
    s.push_str("## Intent\n\n");
    s.push_str(intent);
    s.push_str("\n\n");

    s.push_str("## Signature\n\n");
    s.push_str("```rust\n");
    s.push_str(&format!(
        "async fn {method_snake}(&self, request: Request<{pascal}{method}Request>) -> Result<Response<{pascal}{method}Response>, Status>\n",
        pascal = h.pascal,
        method = event_to_method(event),
    ));
    s.push_str("```\n\n");

    s.push_str("## Request fields\n\n");
    s.push_str("| field | type | required |\n");
    s.push_str("|---|---|---|\n");
    s.push_str("| operation | string | yes (system) |\n");
    s.push_str("| user_id | optional string | no (system) |\n");
    s.push_str("| entity_id | optional string | no (system) |\n");
    for (name, ty, required) in &scalar_fields {
        s.push_str(&format!(
            "| {name} | {ty} | {} |\n",
            if *required { "yes" } else { "no" }
        ));
    }
    s.push_str("\n## Response fields\n\n");
    s.push_str("- `abort_reason: optional string` — set to abort the operation.\n");
    s.push_str("- Any field listed above (all optional) — set to overwrite that\n");
    s.push_str("  field in the entity payload before persistence.\n\n");

    s.push_str("## Done when\n\n");
    s.push_str("- [ ] `cargo check` succeeds in this project.\n");
    s.push_str("- [ ] Happy path returns `abort_reason = None` and the\n");
    s.push_str("      desired modified fields.\n");
    s.push_str("- [ ] Edge cases (malformed input, downstream failures) are\n");
    s.push_str("      handled without panics.\n");
    s
}

fn event_to_method(event: HookEvent) -> &'static str {
    match event {
        HookEvent::BeforeValidate => "BeforeValidate",
        HookEvent::BeforeChange => "BeforeChange",
        HookEvent::AfterChange => "AfterChange",
        HookEvent::BeforeRead => "BeforeRead",
        HookEvent::AfterRead => "AfterRead",
        HookEvent::BeforeDelete => "BeforeDelete",
        HookEvent::AfterDelete => "AfterDelete",
    }
}

// ---------------------------------------------------------------------------
// hooks list
// ---------------------------------------------------------------------------

fn list(
    args: HooksListArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas(std::slice::from_ref(&args.schema_dir))?;
    let mut found = 0;
    for def in &schemas {
        let hooks: Vec<&Annotation> = def
            .annotations
            .iter()
            .filter(|a| matches!(a, Annotation::Hook { .. }))
            .collect();
        if hooks.is_empty() {
            continue;
        }
        output.status(&format!("schema {}", def.name.as_str()));
        for h in hooks {
            if let Annotation::Hook { event, intent } = h {
                output.status(&format!("  {} — {intent}", event.as_str()));
                found += 1;
            }
        }
    }
    if found == 0 {
        output.warn("no @hook(...) annotations found");
    } else {
        output.status(&format!("{found} hook(s) total"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// hooks diff
// ---------------------------------------------------------------------------

fn diff(
    args: HooksDiffArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let old = parse_all_schemas(std::slice::from_ref(&args.old))?;
    let new = parse_all_schemas(std::slice::from_ref(&args.new))?;

    let old_map = build_hook_map(&old);
    let new_map = build_hook_map(&new);

    let mut any = false;
    let all_keys: std::collections::BTreeSet<_> =
        old_map.keys().chain(new_map.keys()).collect();

    for key in all_keys {
        let (schema, event) = key;
        match (old_map.get(key), new_map.get(key)) {
            (None, Some(intent)) => {
                output.status(&format!("+ {schema}.{} — {intent}", event.as_str()));
                any = true;
            }
            (Some(_), None) => {
                output.status(&format!("- {schema}.{}", event.as_str()));
                any = true;
            }
            (Some(old_intent), Some(new_intent)) if old_intent != new_intent => {
                output.status(&format!(
                    "~ {schema}.{} (intent changed)",
                    event.as_str()
                ));
                any = true;
            }
            _ => {}
        }
    }
    if !any {
        output.status("no hook changes");
    }
    Ok(())
}

fn build_hook_map(
    schemas: &[SchemaDefinition],
) -> BTreeMap<(String, HookEvent), String> {
    let mut map = BTreeMap::new();
    for s in schemas {
        for a in &s.annotations {
            if let Annotation::Hook { event, intent } = a {
                map.insert((s.name.as_str().to_string(), *event), intent.clone());
            }
        }
    }
    map
}

// Keep `_global` accessible without warning suppression by re-exporting if
// future subcommands need it.
#[allow(dead_code)]
fn _placeholder(_: PathBuf) {}
