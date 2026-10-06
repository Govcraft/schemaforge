//! CLI adapter for portable site generation.
use crate::cli::{GlobalOpts, SiteCommands, SiteGenerateArgs};
use crate::commands::codegen::{check_plan, write_plan, SentinelKind, WriteOptions};
use crate::commands::parse::parse_all_schemas_with_global;
use crate::error::CliError;
use crate::output::OutputContext;
use schema_forge_codegen::site::{plan_site, SiteOptions};

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
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    output.status(&format!(
        "Scanning schemas in {}...",
        args.schema_dir.display()
    ));
    let schemas =
        parse_all_schemas_with_global(std::slice::from_ref(&args.schema_dir), global, output)?;
    let config = crate::config::load_svc_config(global)?;
    let options = SiteOptions {
        schema_dir: args.schema_dir,
        schema: args.schema,
        name: args.name,
        title_suffix: args.title_suffix,
        logo: args.logo,
        logo_on_dark: args.logo_on_dark,
        favicon: args.favicon,
        templates_dir: args.templates_dir,
        accessibility_contact: args.accessibility_contact,
        config_path: global.config.clone(),
    };
    let plan = plan_site(&schemas, &options, &config.custom.schema_forge.site)?;
    for warning in &plan.warnings {
        output.warn(warning);
    }
    let write_options = WriteOptions {
        generator: "site",
        sentinel_kind: SentinelKind::Site,
        force_user_files: args.force_user_files,
        force_init: args.force_init,
    };
    if args.check {
        let report = check_plan(&args.out_dir, &plan.files, write_options)?;
        if report.is_clean() {
            output.success("site generator is idempotent, no drift");
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
                report.orphaned.len()
            ),
        });
    }
    write_plan(&args.out_dir, &plan.files, write_options)?;
    output.success(&format!(
        "React site scaffold written to {} ({} entities)",
        args.out_dir.display(),
        plan.entity_count
    ));
    output.status("  Next steps:");
    output.status(&format!(
        "    cd {} && pnpm install && pnpm build",
        args.out_dir.display()
    ));
    output.status("    pnpm dev  # local preview");
    Ok(())
}
