//! `schema-forge site` subcommand: generate a Vite + React + Tailwind + shadcn
//! project from a schema definition.
//!
//! v0 spike — one entity deep, end-to-end: list / detail / edit pages backed
//! by a typed REST client, Zod validators, and a shadcn UI. Everything hangs
//! off the shared [`commands::codegen`][super::codegen] module so write/check
//! semantics, marker verification, sentinel guarding, and manifest pruning
//! behave identically to the `hooks generate` command.

mod context;
mod mapping;
mod render;
mod vendor;

use std::path::PathBuf;

use heck::ToKebabCase;
use schema_forge_core::types::SchemaDefinition;

use crate::cli::{GlobalOpts, SiteCommands, SiteGenerateArgs};
use crate::commands::codegen::{
    check_plan, write_plan, FilePlan, SentinelKind, WriteMode, WriteOptions,
};
use crate::commands::parse::parse_all_schemas;
use crate::error::CliError;
use crate::output::OutputContext;

use self::context::{EntityView, SiteContext};
use self::render::SiteRenderer;

/// Generator identifier embedded in markers and the manifest.
const GENERATOR: &str = "site";

/// Top-level dispatch for `schema-forge site ...`.
pub async fn run(
    command: SiteCommands,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    match command {
        SiteCommands::Generate(args) => generate(args, global, output),
    }
}

fn generate(
    args: SiteGenerateArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.status(&format!(
        "Scanning schemas in {}...",
        args.schema_dir.display()
    ));
    let schemas = parse_all_schemas(std::slice::from_ref(&args.schema_dir))?;

    if schemas.is_empty() {
        return Err(CliError::Config {
            message: format!(
                "no schemas found in {} — nothing to generate",
                args.schema_dir.display()
            ),
        });
    }

    let target = pick_target_schema(&schemas, args.schema.as_deref())?;
    output.status(&format!("  target schema: {}", target.name.as_str()));

    let project_name = args
        .out_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("schema-forge-site")
        .to_kebab_case();

    let entity = EntityView::from_schema(target, output)?;
    if entity.fields.is_empty() {
        return Err(CliError::Config {
            message: format!(
                "schema `{}` has no v0-supported fields — everything was skipped. \
                 v0 supports: Text, Integer, Float, Boolean, DateTime, Enum, Relation(One).",
                target.name.as_str(),
            ),
        });
    }

    let ctx = SiteContext {
        project_name,
        entity,
    };

    let renderer = SiteRenderer::new()?;
    let plan = build_plan(&ctx, &renderer)?;

    let options = WriteOptions {
        generator: GENERATOR,
        sentinel_kind: SentinelKind::Site,
        force_user_files: args.force_user_files,
        force_init: args.force_init,
    };

    if args.check {
        let report = check_plan(&args.out_dir, &plan, options)?;
        if report.is_clean() {
            output.success("site generator is idempotent — no drift");
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
        "React site scaffold written to {}",
        args.out_dir.display()
    ));
    output.status("  Next steps:");
    output.status(&format!("    cd {} && pnpm install && pnpm build", args.out_dir.display()));
    output.status("    pnpm dev  # local preview");

    Ok(())
}

fn pick_target_schema<'a>(
    schemas: &'a [SchemaDefinition],
    wanted: Option<&str>,
) -> Result<&'a SchemaDefinition, CliError> {
    match wanted {
        Some(name) => schemas
            .iter()
            .find(|s| s.name.as_str() == name)
            .ok_or_else(|| CliError::Config {
                message: format!(
                    "schema `{name}` not found. Available: {}",
                    schemas
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        None => schemas.first().ok_or_else(|| CliError::Config {
            message: "no schemas found".to_string(),
        }),
    }
}

/// Build the flat [`FilePlan`] list describing every file the site generator
/// wants to produce. Pure function — no I/O beyond template rendering.
fn build_plan(ctx: &SiteContext, renderer: &SiteRenderer) -> Result<Vec<FilePlan>, CliError> {
    let mut plan: Vec<FilePlan> = Vec::with_capacity(32);

    // ---- Project-root owned files (static + templated) ----
    plan.push(owned("package.json", renderer.render("package.json", ctx)?));
    plan.push(owned("vite.config.ts", renderer.render("vite.config.ts", ctx)?));
    plan.push(owned("index.html", renderer.render("index.html", ctx)?));
    plan.push(owned("tailwind.config.ts", renderer.render("tailwind.config.ts", ctx)?));
    plan.push(owned("tsconfig.json", vendor::TSCONFIG_JSON.to_string()));
    plan.push(owned("tsconfig.node.json", vendor::TSCONFIG_NODE_JSON.to_string()));
    plan.push(owned(".gitignore", vendor::GITIGNORE.to_string()));

    // ---- src/ scaffolding ----
    plan.push(owned("src/main.tsx", renderer.render("src/main.tsx", ctx)?));
    plan.push(owned("src/App.tsx", renderer.render("src/App.tsx", ctx)?));
    plan.push(owned("src/index.css", vendor::INDEX_CSS.to_string()));
    plan.push(owned("src/lib/utils.ts", vendor::SHADCN_UTILS_TS.to_string()));

    // ---- shadcn primitives (vendored, owned, unmodified) ----
    plan.push(owned("src/components/ui/button.tsx", vendor::SHADCN_BUTTON.to_string()));
    plan.push(owned("src/components/ui/input.tsx", vendor::SHADCN_INPUT.to_string()));
    plan.push(owned("src/components/ui/label.tsx", vendor::SHADCN_LABEL.to_string()));
    plan.push(owned("src/components/ui/card.tsx", vendor::SHADCN_CARD.to_string()));
    plan.push(owned("src/components/ui/form.tsx", vendor::SHADCN_FORM.to_string()));
    plan.push(owned("src/components/ui/table.tsx", vendor::SHADCN_TABLE.to_string()));

    // ---- Generated per-entity code ----
    plan.push(owned(
        "src/generated/api-client.ts",
        renderer.render("src/generated/api-client.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/entity-types.ts",
        renderer.render("src/generated/entity-types.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/zod-schemas.ts",
        renderer.render("src/generated/zod-schemas.ts", ctx)?,
    ));
    plan.push(owned(
        "src/generated/route-manifest.ts",
        renderer.render("src/generated/route-manifest.ts", ctx)?,
    ));

    // ---- pages/<entity>/ (Preserve: scaffold-once) ----
    let page_dir = format!("src/pages/{}", ctx.entity.kebab);
    plan.push(preserve(
        &format!("{page_dir}/list.tsx"),
        renderer.render("src/pages/list.tsx", ctx)?,
    ));
    plan.push(preserve(
        &format!("{page_dir}/detail.tsx"),
        renderer.render("src/pages/detail.tsx", ctx)?,
    ));
    plan.push(preserve(
        &format!("{page_dir}/edit.tsx"),
        renderer.render("src/pages/edit.tsx", ctx)?,
    ));

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

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        FieldDefinition, FieldModifier, FieldName, FieldType, IntegerConstraints, SchemaId,
        SchemaName, TextConstraints,
    };

    fn employee_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("full_name").unwrap(),
                    FieldType::Text(TextConstraints::with_max_length(255)),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("age").unwrap(),
                    FieldType::Integer(IntegerConstraints::unconstrained()),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn pick_target_defaults_to_first() {
        let s = vec![employee_schema()];
        let t = pick_target_schema(&s, None).unwrap();
        assert_eq!(t.name.as_str(), "Employee");
    }

    #[test]
    fn pick_target_errors_on_unknown_name() {
        let s = vec![employee_schema()];
        let err = pick_target_schema(&s, Some("Nope")).unwrap_err();
        assert!(matches!(err, CliError::Config { .. }));
    }
}
