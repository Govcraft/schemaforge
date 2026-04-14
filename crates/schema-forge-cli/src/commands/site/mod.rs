//! `schema-forge site` subcommand: generate a Vite + React + Tailwind + shadcn
//! project from a schema definition.
//!
//! v1 generator — produces pages for every non-system schema in the schema
//! directory, with shared generated code (types, Zod validators, API client,
//! route manifest) and per-entity pages. `--schema NAME` narrows generation
//! to a single schema for debugging or partial regen.

mod context;
mod mapping;
mod render;
mod vendor;

use std::collections::BTreeMap;
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

use self::context::{EntityView, PageContext, SchemaMeta, SiteContext};
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

    // Filter: drop system schemas (internal control-plane tables like
    // Theme / Workflow) and, if --schema was passed, narrow to that one.
    let targets = pick_target_schemas(&schemas, args.schema.as_deref())?;
    for def in &targets {
        output.status(&format!("  target: {}", def.name.as_str()));
    }

    let project_name = args
        .out_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("schema-forge-site")
        .to_kebab_case();

    // Build a catalog of every known schema so mapping can resolve
    // relation targets (display field, kebab slug) even when the target
    // itself isn't being rendered in this run.
    let catalog: BTreeMap<String, SchemaMeta> = schemas
        .iter()
        .map(|def| (def.name.as_str().to_string(), SchemaMeta::from_schema(def)))
        .collect();

    // Project each schema into an EntityView. Drop entities with zero
    // v0-supported fields so we never emit a broken page.
    let mut entities: Vec<EntityView> = Vec::with_capacity(targets.len());
    for def in &targets {
        let ev = EntityView::from_schema(def, &catalog, output)?;
        if ev.fields.is_empty() {
            output.warn(&format!(
                "site: skipping schema `{}` — no supported fields",
                def.name.as_str(),
            ));
            continue;
        }
        entities.push(ev);
    }

    if entities.is_empty() {
        return Err(CliError::Config {
            message: "no schemas have any v0-supported fields — everything was \
                 skipped. v1 supports: Text, RichText, Integer, Float, \
                 Boolean, DateTime, Enum, Json, Relation(One|Many), \
                 Array(scalar|enum), Composite."
                .to_string(),
        });
    }

    let ctx = SiteContext {
        project_name: project_name.clone(),
        entities,
    };

    let templates_dir = args.templates_dir.clone().or_else(|| {
        let default = PathBuf::from("site-templates");
        default.is_dir().then_some(default)
    });
    if let Some(ref dir) = templates_dir {
        output.status(&format!("Using template overrides from {}", dir.display()));
    }
    let renderer = SiteRenderer::new(templates_dir)?;
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
        "React site scaffold written to {} ({} entities)",
        args.out_dir.display(),
        ctx.entities.len(),
    ));
    output.status("  Next steps:");
    output.status(&format!(
        "    cd {} && pnpm install && pnpm build",
        args.out_dir.display()
    ));
    output.status("    pnpm dev  # local preview");

    Ok(())
}

/// Choose which schemas to generate pages for.
///
/// - System schemas (`@system`) are always excluded — they are
///   control-plane tables, not user-facing data.
/// - If `wanted` is `Some(name)`, only that schema is returned (still
///   subject to the system-schema exclusion).
/// - Otherwise every non-system schema is returned, in DSL declaration order.
fn pick_target_schemas<'a>(
    schemas: &'a [SchemaDefinition],
    wanted: Option<&str>,
) -> Result<Vec<&'a SchemaDefinition>, CliError> {
    match wanted {
        Some(name) => {
            let found = schemas
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
                })?;
            if found.is_system() {
                return Err(CliError::Config {
                    message: format!(
                        "schema `{name}` is a @system schema; system schemas \
                         are excluded from the site generator."
                    ),
                });
            }
            Ok(vec![found])
        }
        None => {
            let all: Vec<&SchemaDefinition> = schemas.iter().filter(|s| !s.is_system()).collect();
            if all.is_empty() {
                return Err(CliError::Config {
                    message: "every schema in the directory is @system; \
                              nothing to generate."
                        .to_string(),
                });
            }
            Ok(all)
        }
    }
}

/// Build the flat [`FilePlan`] list describing every file the site generator
/// wants to produce. Pure function — no I/O beyond template rendering.
fn build_plan(ctx: &SiteContext, renderer: &SiteRenderer) -> Result<Vec<FilePlan>, CliError> {
    let mut plan: Vec<FilePlan> = Vec::with_capacity(32 + 3 * ctx.entities.len());

    // ---- Project-root user files (Preserve: scaffold once) ----
    //
    // package.json has no comment syntax, so we can't embed a `@generated`
    // marker to protect against user edits. Preserve mode scaffolds it
    // once and then lets the user run `pnpm add` freely without the
    // generator clobbering them on regen.
    plan.push(preserve(
        "package.json",
        renderer.render("package.json", ctx)?,
    ));
    plan.push(owned(
        "vite.config.ts",
        renderer.render("vite.config.ts", ctx)?,
    ));
    plan.push(owned("index.html", renderer.render("index.html", ctx)?));
    plan.push(owned(
        "tailwind.config.ts",
        renderer.render("tailwind.config.ts", ctx)?,
    ));
    plan.push(owned("tsconfig.json", vendor::TSCONFIG_JSON.to_string()));
    plan.push(owned(
        "tsconfig.node.json",
        vendor::TSCONFIG_NODE_JSON.to_string(),
    ));
    plan.push(owned(".gitignore", vendor::GITIGNORE.to_string()));

    // ---- src/ scaffolding ----
    plan.push(owned("src/main.tsx", renderer.render("src/main.tsx", ctx)?));
    plan.push(owned("src/App.tsx", renderer.render("src/App.tsx", ctx)?));
    plan.push(owned("src/index.css", vendor::INDEX_CSS.to_string()));
    plan.push(owned(
        "src/lib/utils.ts",
        vendor::SHADCN_UTILS_TS.to_string(),
    ));
    plan.push(owned(
        "src/lib/auth.ts",
        renderer.render("src/lib/auth.ts", ctx)?,
    ));
    plan.push(owned(
        "src/lib/require-auth.tsx",
        renderer.render("src/lib/require-auth.tsx", ctx)?,
    ));

    // ---- shadcn primitives (vendored, owned, unmodified) ----
    plan.push(owned(
        "src/components/ui/button.tsx",
        vendor::SHADCN_BUTTON.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/input.tsx",
        vendor::SHADCN_INPUT.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/label.tsx",
        vendor::SHADCN_LABEL.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/card.tsx",
        vendor::SHADCN_CARD.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/form.tsx",
        vendor::SHADCN_FORM.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/table.tsx",
        vendor::SHADCN_TABLE.to_string(),
    ));
    plan.push(owned(
        "src/components/ui/relation-select.tsx",
        vendor::RELATION_SELECT.to_string(),
    ));

    // ---- Generated multi-entity code (shared across pages) ----
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
    plan.push(owned(
        "src/generated/formatters.ts",
        renderer.render("src/generated/formatters.ts", ctx)?,
    ));

    // ---- Top-level login page (Preserve: users restyle freely) ----
    //
    // Login is mounted at `/login`, outside both the `/app` and `/admin`
    // subtrees, because both need to fall through to it on auth failure.
    plan.push(preserve(
        "src/pages/login.tsx",
        renderer.render("src/pages/login.tsx", ctx)?,
    ));

    // ---- `/app/*`: per-entity user-facing pages (Preserve) ----
    //
    // Lives under `src/app/pages/<kebab>/` so the path mirrors the route
    // tree (`/app/<kebab>`). Users are free to restyle these — subsequent
    // generator runs leave them alone unless `--force-user-files` is set.
    for entity in &ctx.entities {
        let page_ctx = PageContext {
            project_name: ctx.project_name.clone(),
            entity: entity.clone(),
        };
        let page_dir = format!("src/app/pages/{}", entity.kebab);
        plan.push(preserve(
            &format!("{page_dir}/list.tsx"),
            renderer.render("src/app/pages/list.tsx", &page_ctx)?,
        ));
        plan.push(preserve(
            &format!("{page_dir}/detail.tsx"),
            renderer.render("src/app/pages/detail.tsx", &page_ctx)?,
        ));
        plan.push(preserve(
            &format!("{page_dir}/edit.tsx"),
            renderer.render("src/app/pages/edit.tsx", &page_ctx)?,
        ));
    }

    // ---- `/admin/*`: generic schema-aware admin shell (Owned) ----
    //
    // The admin UI is schema-agnostic: it fetches `/api/v1/forge/schemas`
    // at runtime and renders generic CRUD for every schema the authenticated
    // user has permission to see. Because it's not per-user content, these
    // files are Owned — users customize the admin by overriding the templates
    // via `--templates-dir`, not by hand-editing the generated .tsx.
    //
    // Phase 2 ships these as placeholder scaffolds; Phase 3 fills them in.
    for (rel, logical) in ADMIN_TEMPLATES {
        plan.push(owned(rel, renderer.render(logical, ctx)?));
    }

    Ok(plan)
}

/// Generic admin shell files. Each tuple is `(output path, template name)`.
/// Templates live under `templates/site/src/admin/` and are schema-agnostic
/// (rendered once against the top-level `SiteContext`, not per-entity).
const ADMIN_TEMPLATES: &[(&str, &str)] = &[
    ("src/admin/layout.tsx", "src/admin/layout.tsx"),
    ("src/admin/schemas-index.tsx", "src/admin/schemas-index.tsx"),
    ("src/admin/entity-list.tsx", "src/admin/entity-list.tsx"),
    ("src/admin/entity-detail.tsx", "src/admin/entity-detail.tsx"),
    ("src/admin/entity-edit.tsx", "src/admin/entity-edit.tsx"),
    ("src/admin/api-client.ts", "src/admin/api-client.ts"),
    (
        "src/admin/field-renderer.tsx",
        "src/admin/field-renderer.tsx",
    ),
    ("src/admin/users-list.tsx", "src/admin/users-list.tsx"),
    ("src/admin/users-edit.tsx", "src/admin/users-edit.tsx"),
];

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
    fn pick_target_defaults_to_all_non_system() {
        let s = vec![employee_schema()];
        let t = pick_target_schemas(&s, None).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name.as_str(), "Employee");
    }

    #[test]
    fn pick_target_errors_on_unknown_name() {
        let s = vec![employee_schema()];
        let err = pick_target_schemas(&s, Some("Nope")).unwrap_err();
        assert!(matches!(err, CliError::Config { .. }));
    }
}
