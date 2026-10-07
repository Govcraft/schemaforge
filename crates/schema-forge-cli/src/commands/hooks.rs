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
//!   Cargo.toml                         # Preserve — scaffolded once, user edits
//!   build.rs                           # Owned; copies descriptor to project root
//!   hooks_descriptor.bin               # Build artifact (stable path for runtime)
//!   proto/
//!     <schema>_hooks.proto             # Owned, one per annotated schema
//!   src/
//!     main.rs                          # Preserve — scaffolded once, user edits
//!     hooks/
//!       mod.rs                         # Preserve, additive module declarations
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
use crate::commands::parse::parse_all_schemas_with_global;
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
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.status(&format!(
        "Scanning schemas in {}...",
        args.schema_dir.display()
    ));
    let schemas =
        parse_all_schemas_with_global(std::slice::from_ref(&args.schema_dir), global, output)?;
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

    // `--regenerate` subsumes `--force-user-files`: rewriting every
    // Preserve file from scratch is exactly the full-regen escape hatch.
    // See issue #41.
    let options = WriteOptions {
        generator: GENERATOR,
        sentinel_kind: SentinelKind::Hooks,
        force_user_files: args.force_user_files || args.regenerate,
        force_init: args.force_init,
    };

    // Project Preserve changes once so dry-run and write mode agree, including
    // additive wiring, missing scaffolds, legacy upgrades, and forced rewrites.
    let preserved =
        additive::project_preserve_files(&args.out_dir, &plan, &hooked, options, output)?;

    if args.check {
        let mut report = check_plan(&args.out_dir, &plan, options)?;
        for change in &preserved {
            if change.missing {
                report.missing.push(change.relative_path.clone());
            } else {
                report.differing.push(change.relative_path.clone());
            }
        }
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

    let inserted = additive::apply_preserve_changes(&args.out_dir, &preserved)?;
    if !inserted.is_empty() {
        output.status(&format!(
            "  additive: inserted {} net-new schema(s): {}",
            inserted.len(),
            inserted.join(", "),
        ));
    }

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
    // `Cargo.toml`, `build.rs`, `src/main.rs`, and `src/hooks/mod.rs` are
    // all `Preserve`: scaffolded once, then user-owned. Users accumulate
    // dependencies, custom build logic (see #15), env-var validation,
    // per-service constructor wiring, and extra `mod` declarations in
    // these files, and regenerate runs must not clobber them.
    //
    // `main.rs` and `mod.rs` carry stable insertion markers in the
    // scaffolded templates — subsequent runs splice new schemas into
    // those regions surgically without touching anything outside the
    // markers. See [`additive`] and issue #41.
    //
    // `--regenerate` rewrites every Preserve file verbatim; use it as
    // the one-off rescaffold escape hatch.
    let mut plan: Vec<FilePlan> = vec![
        preserve("Cargo.toml", render_cargo_toml(project_name)),
        preserve("config.toml", render_config_toml(project_name)),
        preserve("build.rs", BUILD_RS.to_string()),
        preserve("src/main.rs", render_main_rs(hooked)),
        preserve("src/hooks/mod.rs", render_hooks_mod(hooked)),
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

/// The `acton-service` version the scaffold pins.
///
/// Kept in step with the workspace's own pin: a hook service validates the
/// credentials this forge mints, so the two need the same token
/// implementation. Bump both together.
const SCAFFOLD_ACTON_SERVICE_VERSION: &str = "0.46.0";

fn render_cargo_toml(project_name: &str) -> String {
    format!(
        r#"[package]
name = "{project_name}"
version = "0.1.0"
edition = "2021"

[dependencies]
# The hook service runs on the same platform layer as the forge that calls it.
# This is what supplies token authentication, mutual-TLS caller authorization,
# tracing, and the health surface — see `src/main.rs`. Do not replace it with a
# bare `tonic::transport::Server`: nothing would authenticate the caller.
acton-service = {{ version = "{SCAFFOLD_ACTON_SERVICE_VERSION}", features = ["grpc", "tls"] }}
prost = "0.14"
tokio = {{ version = "1", features = ["full"] }}
tonic = "0.14"
tonic-prost = "0.14"
tracing = "0.1"

[build-dependencies]
tonic-build = "0.14"
tonic-prost-build = "0.14"
"#
    )
}

// ---------------------------------------------------------------------------
// Additive insertion markers
// ---------------------------------------------------------------------------
//
// `main.rs` and `src/hooks/mod.rs` ship as Preserve files: users accumulate
// real customization in them (custom module imports, env-var validation,
// per-service constructor wiring). When a new `@hook`-annotated schema is
// added to the project, the generator must splice new lines into those
// files *without* rewriting everything else. Stable marker comments bound
// the regions that receive schema wiring. PB and module declarations are
// regenerated from the current schema list. Service registrations are retained
// byte for byte while selected, with missing registrations appended and
// obsolete schema registrations removed. Everything outside
// the markers is left alone.
//
// See issue #41 for the motivating scenario.
pub(crate) const MOD_BEGIN: &str =
    "// SCHEMAFORGE_HOOKS_MODS_BEGIN — DO NOT REMOVE (additive insertion marker)";
pub(crate) const MOD_END: &str = "// SCHEMAFORGE_HOOKS_MODS_END";
pub(crate) const PB_BEGIN: &str =
    "    // SCHEMAFORGE_HOOKS_PB_BEGIN — DO NOT REMOVE (additive insertion marker)";
pub(crate) const PB_END: &str = "    // SCHEMAFORGE_HOOKS_PB_END";
pub(crate) const SVC_BEGIN: &str =
    "        // SCHEMAFORGE_HOOKS_SERVICES_BEGIN — DO NOT REMOVE (additive insertion marker)";
pub(crate) const SVC_END: &str = "        // SCHEMAFORGE_HOOKS_SERVICES_END";

fn render_pb_entry(h: &SchemaHooks) -> String {
    let mut s = String::new();
    s.push_str(&format!("    pub mod {} {{\n", h.snake));
    s.push_str(&format!(
        "        tonic::include_proto!(\"{}\");\n",
        h.proto_package
    ));
    s.push_str("    }\n");
    s
}

fn render_service_line(h: &SchemaHooks) -> String {
    format!(
        "        .add_service(pb::{snake}::{snake}_hooks_server::{pascal}HooksServer::new(hooks::{snake}::Service::default()))\n",
        snake = h.snake,
        pascal = h.pascal,
    )
}

fn render_mod_line(h: &SchemaHooks) -> String {
    format!("pub mod {};\n", h.snake)
}

fn render_main_rs(hooked: &[SchemaHooks]) -> String {
    let mut s = String::new();
    s.push_str("//! Scaffolded once by `schema-forge hooks generate`, edit freely.\n");
    s.push_str("//!\n");
    s.push_str("//! Subsequent runs are additive: new `@hook`-annotated schemas get\n");
    s.push_str("//! spliced into `mod pb { ... }` and the `GrpcServicesBuilder` chain\n");
    s.push_str("//! between the `SCHEMAFORGE_HOOKS_*` marker comments below. Keep\n");
    s.push_str("//! those markers in place and your custom module imports, env-var\n");
    s.push_str("//! validation, and per-service constructor wiring will survive\n");
    s.push_str("//! every regen. Use `--regenerate` to opt out and rewrite this\n");
    s.push_str("//! file from scratch.\n");
    s.push_str("//!\n");
    s.push_str("//! # Dependency wiring\n");
    s.push_str("//!\n");
    s.push_str("//! Replace `hooks::<schema>::Service::default()` with your constructor,\n");
    s.push_str("//! for example `hooks::<schema>::Service::new(executor)`, directly in\n");
    s.push_str("//! the `.add_service(...)` registration below. Keep the qualified\n");
    s.push_str("//! `pb::<schema>::<schema>_hooks_server::<Schema>HooksServer` path so\n");
    s.push_str("//! additive generation can recognize the registration. Constructor\n");
    s.push_str("//! arguments and surrounding comments survive subsequent runs unchanged.\n");
    s.push_str("//! New schemas receive a default constructor to customize in the same way.\n");
    s.push_str("//! Removing a schema from generation removes its service registration;\n");
    s.push_str("//! constructors for the remaining schemas stay unchanged.\n");
    s.push_str("//! `--regenerate` and `--force-user-files` reset this wiring to the scaffold.\n");
    s.push_str("//!\n");
    s.push_str("//! # Who is allowed to call this\n");
    s.push_str("//!\n");
    s.push_str("//! A hook invocation carries a snapshot of the entity's fields and the\n");
    s.push_str("//! subject claim of the user whose request triggered it, so this process\n");
    s.push_str("//! must not answer to anyone who can reach its port. Serving through\n");
    s.push_str("//! `ServiceBuilder` is what prevents that: when `config.toml` has a\n");
    s.push_str("//! `[token]` section, acton-service applies token authentication to\n");
    s.push_str("//! every registered gRPC service automatically, and the forge presents\n");
    s.push_str("//! a short-lived PASETO minted from the same key. Add `[caller_auth]`\n");
    s.push_str("//! on top to require a specific mutual-TLS SAN.\n");
    s.push_str("//!\n");
    s.push_str("//! Replacing this with a bare `tonic::transport::Server` removes all of\n");
    s.push_str("//! that silently — the RPCs keep working, they just stop being\n");
    s.push_str("//! authenticated.\n\n");
    s.push_str("mod hooks;\n\n");
    s.push_str("mod pb {\n");
    s.push_str(PB_BEGIN);
    s.push('\n');
    for h in hooked {
        s.push_str(&render_pb_entry(h));
    }
    s.push_str(PB_END);
    s.push('\n');
    s.push_str("}\n\n");
    s.push_str("use acton_service::grpc::server::GrpcServicesBuilder;\n");
    s.push_str("use acton_service::prelude::*;\n\n");
    s.push_str("#[tokio::main]\n");
    s.push_str("async fn main() -> Result<()> {\n");
    s.push_str("    // Reads ./config.toml, then $XDG_CONFIG_HOME and /etc; `ACTON_*`\n");
    s.push_str("    // environment variables override the file. The `[grpc]` section must\n");
    s.push_str("    // set `enabled = true` or the build below is refused rather than\n");
    s.push_str("    // silently serving no RPCs. The service has no custom config section,\n");
    s.push_str("    // so it loads `Config<()>`; `GrpcServicesBuilder::build` is generic\n");
    s.push_str("    // over the state type and cannot infer it.\n");
    s.push_str("    let config = Config::<()>::load()?;\n\n");
    s.push_str("    // The health service probes whatever dependencies the config declares.\n");
    s.push_str("    let state = AppState::builder().config(config.clone()).build().await?;\n\n");
    s.push_str("    // Reflection is deliberately not enabled: it would publish every\n");
    s.push_str("    // hook message definition, and therefore your entity field names, to\n");
    s.push_str("    // unauthenticated callers — reflection and health are exempt from the\n");
    s.push_str("    // token layer. Turn it on with `.with_reflection()` plus\n");
    s.push_str("    // `.add_file_descriptor_set(..)` only where that exposure is fine.\n");
    s.push_str("    let grpc_services = GrpcServicesBuilder::new()\n");
    s.push_str("        .with_health()\n");
    s.push_str(SVC_BEGIN);
    s.push('\n');
    for h in hooked {
        s.push_str(&render_service_line(h));
    }
    s.push_str(SVC_END);
    s.push('\n');
    s.push_str("        .build(Some(state));\n\n");
    s.push_str("    ServiceBuilder::new()\n");
    s.push_str("        .with_config(config)\n");
    s.push_str("        .with_grpc_services(grpc_services)\n");
    s.push_str("        .try_build()?\n");
    s.push_str("        .serve()\n");
    s.push_str("        .await?;\n");
    s.push_str("    Ok(())\n");
    s.push_str("}\n");
    s
}

/// Starter `config.toml` for the scaffolded hook service.
///
/// Emitted with `[token]` already present rather than commented out, because a
/// commented-out auth section is the same as no auth section: the service would
/// start, serve, and answer every caller, and nothing in its output would say
/// so. The key path is a placeholder that fails loudly at startup if it has not
/// been pointed at the forge's key.
fn render_config_toml(project_name: &str) -> String {
    format!(
        r#"# Configuration for the {project_name} hook service.
#
# Loaded by `Config::<()>::load()` in src/main.rs from this file, then from
# $XDG_CONFIG_HOME and /etc. `ACTON_*` environment variables override it.

[service]
name = "{project_name}"
# 0.0.0.0 is safe here only because the surface below is authenticated.
# Narrow it to the interface the forge reaches you on if you can.
bind = "0.0.0.0"
port = 9090

[grpc]
enabled = true
# false multiplexes gRPC onto the HTTP port above, which is what the forge
# dials. Set true only if you also want a separate gRPC listener.
use_separate_port = false

# Token authentication. acton-service applies this to every registered gRPC
# service automatically; removing this section removes that, and the RPCs go
# on working unauthenticated.
#
# `key_path` must be the same PASETO key the forge signs with, since the
# credential on an inbound hook call is minted by the forge. Copy the path
# from the forge's own `[token]` section.
[token]
format = "paseto"
key_path = "/etc/{project_name}/paseto.key"

# Mutual-TLS caller authorization. Uncomment to require that the caller
# present a certificate whose subjectAltName is on the allowlist, in addition
# to a valid token. A private CA authenticates every workload it has ever
# issued to; this is what narrows that to the forge.
#
# [tls]
# enabled = true
# cert_path = "/etc/{project_name}/tls/server.crt"
# key_path = "/etc/{project_name}/tls/server.key"
# client_ca_path = "/etc/{project_name}/tls/ca.crt"
#
# [caller_auth]
# mode = "mtls"
# allowlist = ["schema-forge.internal"]
"#
    )
}

fn render_hooks_mod(hooked: &[SchemaHooks]) -> String {
    let mut s = String::new();
    s.push_str("//! Per-schema hook service implementations.\n");
    s.push_str("//!\n");
    s.push_str("//! Module declarations for annotated schemas are managed additively\n");
    s.push_str("//! between the `SCHEMAFORGE_HOOKS_MODS_*` markers below — keep those\n");
    s.push_str("//! comments in place. Add your own `pub mod` lines outside the\n");
    s.push_str("//! markers if you want them to survive every regen.\n\n");
    s.push_str(MOD_BEGIN);
    s.push('\n');
    for h in hooked {
        s.push_str(&render_mod_line(h));
    }
    s.push_str(MOD_END);
    s.push('\n');
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
        if is_file_event(*event) {
            // File-specific shape: entity-level scalars are omitted because file
            // events fire in isolation from entity writes. Hook services receive
            // the file's declared metadata plus a short-TTL download URL (for
            // `after_upload` / `on_scan_complete`) so they can stream bytes
            // directly from object storage.
            for file_field in file_hook_fields(*event) {
                s.push_str(&file_field.render(tag));
                tag += 1;
            }
        } else {
            for f in &scalar_fields {
                s.push_str(&render_proto_field_line(f, tag, /* request = */ true));
                tag += 1;
            }
        }
        s.push_str("}\n\n");

        // Response — every scalar field is optional (modifiable), repeated
        // fields stay repeated; plus the abort_reason marker.
        s.push_str(&format!("message {}{}Response {{\n", h.pascal, method));
        s.push_str("  optional string abort_reason = 1;\n");
        let mut tag = 100;
        if is_file_event(*event) {
            // File events are fire-and-forget with respect to data modification —
            // only `before_upload` is blocking, and it can only abort, not mutate.
            // Response shape is just the abort marker plus an optional advisory
            // status the hook can return for logging.
            s.push_str("  optional string advisory_status = 100;\n");
        } else {
            for f in &scalar_fields {
                s.push_str(&render_proto_field_line(f, tag, /* request = */ false));
                tag += 1;
            }
        }
        s.push_str("}\n\n");
    }

    Ok(s)
}

/// Fields carried by a file-event hook request. Names match the keys the
/// runtime's `files.rs` handler inserts into `HookInvocation::fields`.
fn file_hook_fields(event: HookEvent) -> &'static [FileHookField] {
    match event {
        HookEvent::BeforeUpload => &[
            FileHookField {
                name: "field_name",
                required: true,
            },
            FileHookField {
                name: "file_name",
                required: true,
            },
            FileHookField {
                name: "mime_type",
                required: true,
            },
            FileHookField {
                name: "file_size",
                required: true, /* int64 */
            },
        ],
        HookEvent::AfterUpload | HookEvent::OnScanComplete => &[
            FileHookField {
                name: "field_name",
                required: true,
            },
            FileHookField {
                name: "object_key",
                required: true,
            },
            FileHookField {
                name: "mime_type",
                required: true,
            },
            FileHookField {
                name: "file_size",
                required: true,
            },
            FileHookField {
                name: "status",
                required: true,
            },
            FileHookField {
                name: "download_url",
                required: false,
            },
        ],
        _ => &[],
    }
}

#[derive(Debug, Clone, Copy)]
struct FileHookField {
    name: &'static str,
    required: bool,
}

impl FileHookField {
    fn render(self, tag: u32) -> String {
        let (ty, optional_prefix) = match self.name {
            "file_size" => ("int64", if self.required { "" } else { "optional " }),
            _ => ("string", if self.required { "" } else { "optional " }),
        };
        format!(
            "  {optional_prefix}{ty} {name} = {tag};\n",
            name = self.name
        )
    }
}

fn is_file_event(event: HookEvent) -> bool {
    matches!(
        event,
        HookEvent::BeforeUpload | HookEvent::AfterUpload | HookEvent::OnScanComplete
    )
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
        // Files are projected as an `optional string` carrying the JSON-encoded
        // attachment snapshot (key, size, mime, status, etc.). File-specific
        // hook events also receive dedicated `file_name`, `mime_type`,
        // `file_size`, `object_key`, `status`, and `download_url` fields; see
        // `file_hook_fields` in render_proto.
        FieldType::File(_) => Ok(("string", false)),
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
        for line in intent.trim().lines() {
            s.push_str("    /// ");
            s.push_str(line.trim_end());
            s.push('\n');
        }
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

// ---------------------------------------------------------------------------
// Additive splice pass (issue #41)
// ---------------------------------------------------------------------------

/// Hook-local projections of user-owned scaffolds. Reads and writes stay at
/// the boundary; the marker transformations are pure and shared by both modes.
mod additive {
    use std::fs;
    use std::path::{Path, PathBuf};

    use heck::ToPascalCase;

    use super::{
        render_hooks_mod, render_main_rs, render_mod_line, render_pb_entry, render_service_line,
        FilePlan, SchemaHooks, WriteMode, WriteOptions, MOD_BEGIN, MOD_END, PB_BEGIN, PB_END,
        SVC_BEGIN, SVC_END,
    };
    use crate::error::CliError;
    use crate::output::OutputContext;

    pub(super) struct PreserveChange {
        pub relative_path: PathBuf,
        pub contents: String,
        pub missing: bool,
    }

    pub(super) fn project_preserve_files(
        out_dir: &Path,
        plan: &[FilePlan],
        hooked: &[SchemaHooks],
        options: WriteOptions,
        output: &OutputContext,
    ) -> Result<Vec<PreserveChange>, CliError> {
        let mut changes = Vec::new();
        for entry in plan
            .iter()
            .filter(|entry| entry.mode == WriteMode::Preserve)
        {
            let path = out_dir.join(&entry.relative_path);
            let existing = read_if_exists(&path)?;
            let desired = match existing.as_deref() {
                None => entry.contents.clone(),
                Some(_) if options.force_user_files => entry.contents.clone(),
                Some(source) => match entry.relative_path.to_str() {
                    Some("src/main.rs") => {
                        report_layout(source, PB_BEGIN, &path, output);
                        project_main_rs(source, hooked, &path)?
                    }
                    Some("src/hooks/mod.rs") => {
                        report_layout(source, MOD_BEGIN, &path, output);
                        project_mod_rs(source, hooked, &path)?
                    }
                    _ => source.to_string(),
                },
            };
            if existing.as_deref() != Some(desired.as_str()) {
                changes.push(PreserveChange {
                    relative_path: entry.relative_path.clone(),
                    contents: desired,
                    missing: existing.is_none(),
                });
            }
        }
        Ok(changes)
    }

    fn report_layout(source: &str, marker: &str, path: &Path, output: &OutputContext) {
        if source.contains(marker) {
            return;
        }
        if source.contains("@generated by schema-forge hooks") {
            output.status(&format!(
                "  additive: upgrading legacy {} to marker-bounded layout",
                path.display()
            ));
        } else {
            output.warn(&format!(
                "additive: {} is missing insertion markers, skipping splice. \
                 Re-run with --regenerate to rewrite from the current scaffold, \
                 or restore the SCHEMAFORGE_HOOKS insertion markers by hand.",
                path.display()
            ));
        }
    }

    pub(super) fn apply_preserve_changes(
        out_dir: &Path,
        changes: &[PreserveChange],
    ) -> Result<Vec<String>, CliError> {
        let mod_path = out_dir.join("src/hooks/mod.rs");
        let before_mods = read_mod_set(&mod_path);
        for change in changes.iter().filter(|change| !change.missing) {
            let path = out_dir.join(&change.relative_path);
            fs::write(&path, &change.contents).map_err(|source| CliError::Io { path, source })?;
        }
        let after_mods = read_mod_set(&mod_path);
        Ok(after_mods.difference(&before_mods).cloned().collect())
    }

    fn project_main_rs(
        existing: &str,
        hooked: &[SchemaHooks],
        path: &Path,
    ) -> Result<String, CliError> {
        if !existing.contains(PB_BEGIN) {
            return Ok(if existing.contains("@generated by schema-forge hooks") {
                render_main_rs(hooked)
            } else {
                existing.to_string()
            });
        }
        let pb_body: String = hooked.iter().map(render_pb_entry).collect();
        let services = &existing[region_range(existing, SVC_BEGIN, SVC_END, path)?];
        let svc_body = project_services(services, hooked);
        let spliced = splice_region(existing, PB_BEGIN, PB_END, &pb_body, path)?;
        splice_region(&spliced, SVC_BEGIN, SVC_END, &svc_body, path)
    }

    fn project_mod_rs(
        existing: &str,
        hooked: &[SchemaHooks],
        path: &Path,
    ) -> Result<String, CliError> {
        if !existing.contains(MOD_BEGIN) {
            return Ok(if existing.contains("@generated by schema-forge hooks") {
                render_hooks_mod(hooked)
            } else {
                existing.to_string()
            });
        }
        let mod_body: String = hooked.iter().map(render_mod_line).collect();
        splice_region(existing, MOD_BEGIN, MOD_END, &mod_body, path)
    }

    struct Registration<'a> {
        schema: &'a str,
        span: std::ops::Range<usize>,
    }

    /// Preserve active calls verbatim, remove obsolete calls without disturbing
    /// surrounding user comments, and append registrations for new schemas.
    fn project_services(source: &str, hooked: &[SchemaHooks]) -> String {
        let registrations = registrations(source);
        let mut projected = String::new();
        let mut copied = 0;
        for registration in &registrations {
            if !hooked
                .iter()
                .any(|schema| schema.snake == registration.schema)
            {
                projected.push_str(&source[copied..registration.span.start]);
                copied = registration.span.end;
            }
        }
        projected.push_str(&source[copied..]);
        for schema in hooked {
            if !registrations.iter().any(|call| call.schema == schema.snake) {
                projected.push_str(&render_service_line(schema));
            }
        }
        projected
    }

    /// Recognize a qualified hook server path inside an actual add_service
    /// argument. Constructor choice and Rust formatting do not affect identity.
    fn registrations(source: &str) -> Vec<Registration<'_>> {
        let tokens = rust_tokens(source);
        let mut registrations = Vec::new();
        let mut index = 0;
        while index + 2 < tokens.len() {
            if tokens[index].text != "."
                || tokens[index + 1].text != "add_service"
                || tokens[index + 2].text != "("
            {
                index += 1;
                continue;
            }
            let arguments = &tokens[index + 3..];
            let mut depth = 1;
            let Some(end) = arguments.iter().position(|token| {
                match token.text {
                    "(" => depth += 1,
                    ")" => depth -= 1,
                    _ => {}
                }
                depth == 0
            }) else {
                break;
            };
            if let Some(schema) = arguments[..end].windows(7).find_map(qualified_schema) {
                registrations.push(Registration {
                    schema,
                    span: tokens[index].offset..arguments[end].offset + 1,
                });
            }
            index += 3 + end + 1;
        }
        registrations
    }

    fn qualified_schema<'a>(path: &[Token<'a>]) -> Option<&'a str> {
        let schema = path[2].text;
        (path[0].text == "pb"
            && path[1].text == "::"
            && path[3].text == "::"
            && path[4].text == format!("{schema}_hooks_server")
            && path[5].text == "::"
            && path[6].text == format!("{}HooksServer", schema.to_pascal_case()))
        .then_some(schema)
    }

    struct Token<'a> {
        text: &'a str,
        offset: usize,
    }

    /// Minimal lexical scan for path recognition, not a Rust parser. Literals
    /// and comments contribute no tokens, including nested block comments and
    /// raw strings. Every other token is retained to avoid joining paths across
    /// punctuation or accidentally interpreting a string as a registration.
    fn rust_tokens(source: &str) -> Vec<Token<'_>> {
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < source.len() {
            let rest = &source[index..];
            if let Some(length) = skipped_token_len(rest) {
                index += length;
                continue;
            }
            let Some(first) = rest.chars().next() else {
                break;
            };
            let length = if rest.starts_with("::") {
                2
            } else if first.is_alphabetic() || first == '_' {
                rest.char_indices()
                    .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                    .map_or(rest.len(), |(offset, _)| offset)
            } else {
                first.len_utf8()
            };
            tokens.push(Token {
                text: &source[index..index + length],
                offset: index,
            });
            index += length;
        }
        tokens
    }

    fn skipped_token_len(source: &str) -> Option<usize> {
        let first = source.chars().next()?;
        if first.is_whitespace() {
            return Some(first.len_utf8());
        }
        if source.starts_with("//") {
            return Some(source.find('\n').unwrap_or(source.len()));
        }
        if source.starts_with("/*") {
            return Some(block_comment_len(source));
        }
        if first == '"' {
            return Some(quoted_literal_len(source, '"'));
        }
        if let Some(after_quote) = source.strip_prefix('\'') {
            let character = after_quote.chars().next()?;
            if character == '\\' || after_quote[character.len_utf8()..].starts_with('\'') {
                return Some(quoted_literal_len(source, '\''));
            }
        }
        raw_literal_len(source)
    }

    fn block_comment_len(source: &str) -> usize {
        let mut depth = 1;
        let mut index = 2;
        while index < source.len() {
            if source[index..].starts_with("/*") {
                depth += 1;
                index += 2;
            } else if source[index..].starts_with("*/") {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return index;
                }
            } else {
                index += source[index..].chars().next().map_or(1, char::len_utf8);
            }
        }
        source.len()
    }

    fn quoted_literal_len(source: &str, quote: char) -> usize {
        let mut escaped = false;
        for (index, character) in source.char_indices().skip(1) {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                return index + character.len_utf8();
            }
        }
        source.len()
    }

    fn raw_literal_len(source: &str) -> Option<usize> {
        let prefix = if source.starts_with("br") || source.starts_with("cr") {
            2
        } else if source.starts_with('r') {
            1
        } else {
            return None;
        };
        let hashes = source[prefix..]
            .bytes()
            .take_while(|byte| *byte == b'#')
            .count();
        let quote = prefix + hashes;
        if source.as_bytes().get(quote) != Some(&b'"') {
            return None;
        }
        let suffix = format!("\"{}", "#".repeat(hashes));
        Some(
            source[quote + 1..]
                .find(&suffix)
                .map_or(source.len(), |end| quote + 1 + end + suffix.len()),
        )
    }

    fn region_range(
        existing: &str,
        begin: &str,
        end: &str,
        path: &Path,
    ) -> Result<std::ops::Range<usize>, CliError> {
        let begin_idx = existing.find(begin).ok_or_else(|| CliError::Config {
            message: format!(
                "{}: additive insertion marker `{begin}` not found",
                path.display()
            ),
        })?;
        let after_begin = begin_idx + begin.len();
        let body_start = if existing.as_bytes().get(after_begin) == Some(&b'\n') {
            after_begin + 1
        } else {
            after_begin
        };
        let end_idx = existing[body_start..]
            .find(end)
            .map(|i| body_start + i)
            .ok_or_else(|| CliError::Config {
                message: format!(
                    "{}: found `{begin}` but no matching `{end}`",
                    path.display()
                ),
            })?;
        Ok(body_start..end_idx)
    }

    /// Replace only the bytes between the marker lines.
    pub(super) fn splice_region(
        existing: &str,
        begin: &str,
        end: &str,
        body: &str,
        path: &Path,
    ) -> Result<String, CliError> {
        let range = region_range(existing, begin, end, path)?;
        let mut out = String::with_capacity(existing.len() + body.len());
        out.push_str(&existing[..range.start]);
        out.push_str(body);
        out.push_str(&existing[range.end..]);
        Ok(out)
    }

    fn read_if_exists(path: &Path) -> Result<Option<String>, CliError> {
        match fs::read_to_string(path) {
            Ok(s) => Ok(Some(s)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CliError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn read_mod_set(path: &Path) -> std::collections::BTreeSet<String> {
        let Ok(source) = fs::read_to_string(path) else {
            return Default::default();
        };
        let Ok(range) = region_range(&source, MOD_BEGIN, MOD_END, path) else {
            return Default::default();
        };
        source[range]
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("pub mod ")
                    .and_then(|rest| rest.strip_suffix(';'))
                    .map(str::to_string)
            })
            .collect()
    }
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
        HookEvent::BeforeUpload => "BeforeUpload",
        HookEvent::AfterUpload => "AfterUpload",
        HookEvent::OnScanComplete => "OnScanComplete",
    }
}

// ---------------------------------------------------------------------------
// hooks list
// ---------------------------------------------------------------------------

fn list(args: HooksListArgs, global: &GlobalOpts, output: &OutputContext) -> Result<(), CliError> {
    let schemas =
        parse_all_schemas_with_global(std::slice::from_ref(&args.schema_dir), global, output)?;
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

fn diff(args: HooksDiffArgs, global: &GlobalOpts, output: &OutputContext) -> Result<(), CliError> {
    let old = parse_all_schemas_with_global(std::slice::from_ref(&args.old), global, output)?;
    let new = parse_all_schemas_with_global(std::slice::from_ref(&args.new), global, output)?;

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
            vec![field(
                "period_of_performance",
                FieldType::Composite(vec![inner]),
            )],
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
