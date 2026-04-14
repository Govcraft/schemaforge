//! `schema-forge hooks` subcommand: generate, list, and diff scaffolds for
//! `@hook(...)` annotations declared in your schemas.
//!
//! The generator emits a self-contained `acton-service` gRPC project rooted
//! at `--out-dir`. Layout:
//!
//! ```text
//! <out-dir>/
//!   .schemaforge-hooks                 # sentinel (zero-byte)
//!   .schemaforge-manifest.toml         # file ownership manifest
//!   Cargo.toml                         # Owned, always rewritten
//!   build.rs                           # Owned; copies descriptor to project root
//!   hooks_descriptor.bin               # Build artifact (stable path for runtime)
//!   proto/
//!     <schema>_hooks.proto             # Owned, one per annotated schema
//!   src/
//!     main.rs                          # Owned
//!     hooks/
//!       mod.rs                         # Owned
//!       <schema>.rs                    # Preserve — scaffold once, user edits
//!       <schema>/
//!         <event>.prompt.md            # Owned prompt file per stub
//! ```
//!
//! File ownership and regeneration behavior are delegated to the shared
//! [`commands::codegen`][super::codegen] module. In short:
//! - `Owned` files are always overwritten (after marker verification),
//!   tracked in the manifest, and pruned if the schema they came from is
//!   deleted.
//! - `Preserve` files are written once, left alone afterwards, and only
//!   rewritten with `--force-user-files`.
//! - `--check` runs the generator in memory and reports drift.

use std::collections::BTreeMap;
use std::path::PathBuf;

use heck::{ToPascalCase, ToSnakeCase};
use schema_forge_core::types::{Annotation, Cardinality, FieldType, HookEvent, SchemaDefinition};

use crate::cli::{GlobalOpts, HooksCommands, HooksDiffArgs, HooksGenerateArgs, HooksListArgs};
use crate::commands::codegen::{
    check_plan, write_plan, FilePlan, SentinelKind, WriteMode, WriteOptions,
};
use crate::commands::parse::parse_all_schemas;
use crate::error::CliError;
use crate::output::OutputContext;

/// Generator identifier embedded in markers and the manifest.
const GENERATOR: &str = "hooks";

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
    let mut hooked: Vec<SchemaHooks> = schemas.into_iter().filter_map(SchemaHooks::from).collect();

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

    output.status(&format!("  found {} schema(s) with hooks", hooked.len()));

    let project_name = args
        .out_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("hooks-service")
        .to_snake_case();
    let plan = build_plan(&project_name, &hooked)?;

    let options = WriteOptions {
        generator: GENERATOR,
        sentinel_kind: SentinelKind::Hooks,
        force_user_files: args.force_user_files,
        force_init: args.force_init,
    };

    if args.check {
        let report = check_plan(&args.out_dir, &plan, options)?;
        if report.is_clean() {
            output.success("hooks generator is idempotent — no drift");
            return Ok(());
        }
        for p in &report.differing {
            output.status(&format!("~ {} (differs)", p.display()));
        }
        for p in &report.missing {
            output.status(&format!("- {} (missing)", p.display()));
        }
        for p in &report.orphaned {
            output.status(&format!("! {} (orphaned)", p.display()));
        }
        return Err(CliError::Config {
            message: format!(
                "check failed: {} differing, {} missing, {} orphaned",
                report.differing.len(),
                report.missing.len(),
                report.orphaned.len(),
            ),
        });
    }

    write_plan(&args.out_dir, &plan, options)?;

    output.success(&format!(
        "Hook service scaffold written to {}",
        args.out_dir.display()
    ));
    output.status("  Next steps:");
    output.status(&format!("    cd {} && cargo check", args.out_dir.display()));
    output.status("    Implement each TODO in src/hooks/<schema>.rs");
    output.status("    Read the .prompt.md files for AI-assist prompts");

    Ok(())
}

/// Build the flat [`FilePlan`] list describing every file the hooks
/// generator wants to produce. Pure function — no I/O, no mutation.
fn build_plan(project_name: &str, hooked: &[SchemaHooks]) -> Result<Vec<FilePlan>, CliError> {
    let mut plan: Vec<FilePlan> = vec![
        owned("Cargo.toml", render_cargo_toml(project_name)),
        owned("build.rs", BUILD_RS.to_string()),
        owned("src/main.rs", render_main_rs(hooked)),
        owned("src/hooks/mod.rs", render_hooks_mod(hooked)),
    ];

    for h in hooked {
        plan.push(owned(
            &format!("proto/{}_hooks.proto", h.snake),
            render_proto(h)?,
        ));
        plan.push(preserve(
            &format!("src/hooks/{}.rs", h.snake),
            render_impl_stub(h),
        ));
        for (event, intent) in &h.events {
            plan.push(owned(
                &format!("src/hooks/{}/{}.prompt.md", h.snake, event.as_str()),
                render_prompt(h, *event, intent)?,
            ));
        }
    }

    Ok(plan)
}

fn owned(path: &str, contents: String) -> FilePlan {
    FilePlan {
        relative_path: PathBuf::from(path),
        contents,
        mode: WriteMode::Owned,
    }
}

fn preserve(path: &str, contents: String) -> FilePlan {
    FilePlan {
        relative_path: PathBuf::from(path),
        contents,
        mode: WriteMode::Preserve,
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

    // Copy the freshly-built descriptor to a stable path at the project root
    // so the schemaforge runtime's `[[schema_forge.hooks.bindings]]` entry can
    // reference `hooks-service/hooks_descriptor.bin` without picking up a stale
    // copy. See schemaforge issue #15.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let stable_path = manifest_dir.join("hooks_descriptor.bin");
    std::fs::copy(&descriptor_path, &stable_path)?;

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

/// Description of a single proto field emitted from a schema field.
#[derive(Debug)]
struct ProtoField {
    name: String,
    /// Proto scalar type (e.g. `"string"`, `"int64"`).
    proto_type: &'static str,
    /// `true` if the field is `required` (used in request messages); ignored
    /// for repeated fields, which are never marked `optional`.
    required: bool,
    /// `true` if the field maps to a `repeated` proto field (DSL array or
    /// `Cardinality::Many` relation).
    repeated: bool,
}

fn render_proto(h: &SchemaHooks) -> Result<String, CliError> {
    let scalar_fields = scalar_proto_fields(&h.schema)?;

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

    for (event, _) in &h.events {
        let method = event_to_method(*event);
        // Request
        s.push_str(&format!("message {}{}Request {{\n", h.pascal, method));
        s.push_str("  string operation = 1;\n");
        s.push_str("  optional string user_id = 2;\n");
        s.push_str("  optional string entity_id = 3;\n");
        let mut tag = 100;
        for f in &scalar_fields {
            s.push_str(&render_proto_field_line(f, tag, /* request = */ true));
            tag += 1;
        }
        s.push_str("}\n\n");

        // Response — every scalar field is optional (modifiable), repeated
        // fields stay repeated; plus the abort_reason marker.
        s.push_str(&format!("message {}{}Response {{\n", h.pascal, method));
        s.push_str("  optional string abort_reason = 1;\n");
        let mut tag = 100;
        for f in &scalar_fields {
            s.push_str(&render_proto_field_line(f, tag, /* request = */ false));
            tag += 1;
        }
        s.push_str("}\n\n");
    }

    Ok(s)
}

/// Format a single proto field line. `request = true` honors the `required`
/// flag (omitting `optional`); `request = false` always emits `optional` for
/// scalars. Repeated fields are always emitted as `repeated <type>`.
fn render_proto_field_line(f: &ProtoField, tag: u32, request: bool) -> String {
    if f.repeated {
        format!(
            "  repeated {ty} {name} = {tag};\n",
            ty = f.proto_type,
            name = f.name
        )
    } else if request && f.required {
        format!("  {ty} {name} = {tag};\n", ty = f.proto_type, name = f.name)
    } else {
        format!(
            "  optional {ty} {name} = {tag};\n",
            ty = f.proto_type,
            name = f.name
        )
    }
}

/// Map a schema's field definitions to [`ProtoField`] descriptors. Returns an
/// error if any field uses a structure protobuf cannot represent without a
/// wrapper message (e.g. nested arrays such as `text[][]`).
fn scalar_proto_fields(schema: &SchemaDefinition) -> Result<Vec<ProtoField>, CliError> {
    use schema_forge_core::types::FieldModifier;
    let mut out = Vec::with_capacity(schema.fields.len());
    for f in &schema.fields {
        let required = f
            .modifiers
            .iter()
            .any(|m| matches!(m, FieldModifier::Required));
        let (proto_type, repeated) =
            field_type_to_proto(&f.field_type, f.name.as_str(), schema.name.as_str())?;
        out.push(ProtoField {
            name: f.name.as_str().to_string(),
            proto_type,
            required,
            repeated,
        });
    }
    Ok(out)
}

/// Map a single [`FieldType`] to `(proto_scalar_type, is_repeated)`.
///
/// Recurses into [`FieldType::Array`] exactly one level. Nested arrays
/// (`text[][]`) and arrays of relations/composites are rejected because
/// protobuf does not support repeated-of-repeated without a wrapper message.
fn field_type_to_proto(
    ft: &FieldType,
    field_name: &str,
    schema_name: &str,
) -> Result<(&'static str, bool), CliError> {
    match ft {
        FieldType::Text(_) | FieldType::RichText => Ok(("string", false)),
        FieldType::Integer(_) => Ok(("int64", false)),
        FieldType::Float(_) => Ok(("double", false)),
        FieldType::Boolean => Ok(("bool", false)),
        FieldType::DateTime => Ok(("string", false)),
        FieldType::Enum(_) => Ok(("string", false)),
        FieldType::Json => Ok(("string", false)),
        // Composites are projected as JSON-stringified `optional string` on the
        // wire. Hook services receive the raw JSON and must parse it themselves.
        // Matches the legacy generator's behavior (see issue #14).
        FieldType::Composite(_) => Ok(("string", false)),
        FieldType::Relation { cardinality, .. } => {
            Ok(("string", matches!(cardinality, Cardinality::Many)))
        }
        FieldType::Array(inner) => {
            // One level only: recurse into the inner type and forbid nesting.
            let (inner_type, inner_repeated) = scalar_inner(inner, field_name, schema_name)?;
            if inner_repeated {
                return Err(CliError::Config {
                    message: format!(
                        "schema `{schema_name}` field `{field_name}`: nested arrays \
                         (e.g. `text[][]`) are not supported by the hooks proto generator; \
                         protobuf has no native repeated-of-repeated. Wrap the inner array \
                         in a composite or restructure the schema.",
                    ),
                });
            }
            Ok((inner_type, true))
        }
        // `FieldType` is `#[non_exhaustive]`. Future variants must be
        // explicitly added to the proto generator.
        other => Err(CliError::Config {
            message: format!(
                "schema `{schema_name}` field `{field_name}`: unsupported field type \
                 `{other}` for hooks proto generation",
            ),
        }),
    }
}

/// Inner-array helper: same as [`field_type_to_proto`] but rejects arrays of
/// relations because the proto cardinality of a `Relation::Many` already
/// implies `repeated`, which would collide with the outer array's `repeated`.
fn scalar_inner(
    ft: &FieldType,
    field_name: &str,
    schema_name: &str,
) -> Result<(&'static str, bool), CliError> {
    match ft {
        FieldType::Relation {
            cardinality: Cardinality::Many,
            ..
        } => Err(CliError::Config {
            message: format!(
                "schema `{schema_name}` field `{field_name}`: array of many-relations \
                 is ambiguous; use a single `-> Foo[]` instead of nesting `[]`.",
            ),
        }),
        other => field_type_to_proto(other, field_name, schema_name),
    }
}

fn render_impl_stub(h: &SchemaHooks) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "//! Service impl for `{}` — generated stub.\n",
        h.name
    ));
    s.push_str("//!\n");
    s.push_str("//! Re-running `schema-forge hooks generate` does NOT overwrite this\n");
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

fn render_prompt(h: &SchemaHooks, event: HookEvent, intent: &str) -> Result<String, CliError> {
    let method_snake = event.as_str();
    let scalar_fields = scalar_proto_fields(&h.schema)?;
    let mut s = String::new();
    s.push_str(&format!("# `{}` — `{}`\n\n", h.name, method_snake));
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
    for f in &scalar_fields {
        let ty_display = if f.repeated {
            format!("repeated {}", f.proto_type)
        } else {
            f.proto_type.to_string()
        };
        s.push_str(&format!(
            "| {name} | {ty} | {req} |\n",
            name = f.name,
            ty = ty_display,
            req = if f.required { "yes" } else { "no" },
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
    Ok(s)
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

fn list(args: HooksListArgs, _global: &GlobalOpts, output: &OutputContext) -> Result<(), CliError> {
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

fn diff(args: HooksDiffArgs, _global: &GlobalOpts, output: &OutputContext) -> Result<(), CliError> {
    let old = parse_all_schemas(std::slice::from_ref(&args.old))?;
    let new = parse_all_schemas(std::slice::from_ref(&args.new))?;

    let old_map = build_hook_map(&old);
    let new_map = build_hook_map(&new);

    let mut any = false;
    let all_keys: std::collections::BTreeSet<_> = old_map.keys().chain(new_map.keys()).collect();

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
                output.status(&format!("~ {schema}.{} (intent changed)", event.as_str()));
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

fn build_hook_map(schemas: &[SchemaDefinition]) -> BTreeMap<(String, HookEvent), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Cardinality, EnumVariants, FieldDefinition, FieldName, FieldType, IntegerConstraints,
        SchemaDefinition, SchemaId, SchemaName, TextConstraints,
    };

    fn field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::new(FieldName::new(name).unwrap(), ft)
    }

    fn schema(name: &str, fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            fields,
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn array_of_text_maps_to_repeated_string() {
        let s = schema(
            "Task",
            vec![field(
                "tags",
                FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            )],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "tags");
        assert_eq!(fields[0].proto_type, "string");
        assert!(fields[0].repeated);
    }

    #[test]
    fn array_of_integer_maps_to_repeated_int64() {
        let s = schema(
            "Task",
            vec![field(
                "scores",
                FieldType::Array(Box::new(FieldType::Integer(
                    IntegerConstraints::unconstrained(),
                ))),
            )],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert_eq!(fields[0].proto_type, "int64");
        assert!(fields[0].repeated);
    }

    #[test]
    fn array_of_enum_maps_to_repeated_string() {
        let s = schema(
            "Task",
            vec![field(
                "labels",
                FieldType::Array(Box::new(FieldType::Enum(
                    EnumVariants::new(vec!["a".into(), "b".into()]).unwrap(),
                ))),
            )],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert_eq!(fields[0].proto_type, "string");
        assert!(fields[0].repeated);
    }

    #[test]
    fn many_relation_maps_to_repeated_string() {
        let s = schema(
            "Task",
            vec![field(
                "projects",
                FieldType::Relation {
                    target: SchemaName::new("Project").unwrap(),
                    cardinality: Cardinality::Many,
                },
            )],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert_eq!(fields[0].proto_type, "string");
        assert!(fields[0].repeated);
    }

    #[test]
    fn one_relation_stays_scalar() {
        let s = schema(
            "Task",
            vec![field(
                "owner",
                FieldType::Relation {
                    target: SchemaName::new("User").unwrap(),
                    cardinality: Cardinality::One,
                },
            )],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert!(!fields[0].repeated);
    }

    #[test]
    fn composite_field_maps_to_scalar_string() {
        let inner = field(
            "base_months",
            FieldType::Integer(IntegerConstraints::unconstrained()),
        );
        let s = schema(
            "Opportunity",
            vec![field("period_of_performance", FieldType::Composite(vec![inner]))],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "period_of_performance");
        assert_eq!(fields[0].proto_type, "string");
        assert!(!fields[0].repeated);
    }

    #[test]
    fn array_of_composite_maps_to_repeated_string() {
        let inner = field("k", FieldType::Text(TextConstraints::unconstrained()));
        let s = schema(
            "Thing",
            vec![field(
                "rows",
                FieldType::Array(Box::new(FieldType::Composite(vec![inner]))),
            )],
        );
        let fields = scalar_proto_fields(&s).unwrap();
        assert_eq!(fields[0].proto_type, "string");
        assert!(fields[0].repeated);
    }

    #[test]
    fn nested_array_is_rejected_with_clear_error() {
        let s = schema(
            "Bad",
            vec![field(
                "matrix",
                FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Text(
                    TextConstraints::unconstrained(),
                ))))),
            )],
        );
        let err = scalar_proto_fields(&s).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("nested arrays"),
            "expected nested-arrays error, got: {msg}"
        );
    }
}
