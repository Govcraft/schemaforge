use std::fs;
use std::path::PathBuf;

use schema_forge_acton::authz::role_ranks::RoleRanks;
use schema_forge_acton::authz::store::PolicyStoreSnapshot;
use schema_forge_acton::cedar::generate_cedar_policies;
use schema_forge_core::inverse_relations::pair_inverse_relations;
use schema_forge_core::system_schemas::all_system_schemas;
use schema_forge_core::types::SchemaDefinition;

use crate::cli::{
    GlobalOpts, PolicyCommands, PolicyListArgs, PolicyRegenerateArgs, PolicyValidateArgs,
};
use crate::commands::parse::parse_all_schemas_with_global;
use crate::commands::schema_update::merge_schema_definitions;
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `policies` subcommand.
pub async fn run(
    command: PolicyCommands,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    match command {
        PolicyCommands::List(args) => run_list(args, global, output).await,
        PolicyCommands::Regenerate(args) => run_regenerate(args, global, output).await,
        PolicyCommands::Validate(args) => run_validate(args, global, output).await,
    }
}

async fn run_list(
    args: PolicyListArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    // For list, we need schemas. Parse from default location.
    let schemas =
        parse_all_schemas_with_global(&[std::path::PathBuf::from("schemas/")], global, output)?;

    for schema in &schemas {
        if let Some(ref filter) = args.schema {
            if schema.name.as_str() != filter {
                continue;
            }
        }

        let policies = generate_cedar_policies(schema);

        match output.mode {
            OutputMode::Human => {
                println!("Cedar policies for {}:", schema.name.as_str());
                println!();
                for (i, policy) in policies.iter().enumerate() {
                    println!("  {}. {}", i + 1, policy.description);
                }
                println!();
            }
            OutputMode::Json => {
                let json_policies: Vec<serde_json::Value> = policies
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "description": p.description,
                            "cedar_text": p.cedar_text,
                        })
                    })
                    .collect();
                let json = serde_json::json!({
                    "schema": schema.name.as_str(),
                    "policies": json_policies,
                });
                output.print_json(&json);
            }
            OutputMode::Plain => {
                for policy in &policies {
                    println!("{}\t{}", schema.name.as_str(), policy.description);
                }
            }
        }
    }

    if let Some(filter) = &args.schema {
        if !schemas.iter().any(|s| s.name.as_str() == filter) {
            return Err(CliError::SchemaNotFound {
                name: filter.clone(),
            });
        }
    }

    output.success("Use 'schema-forge policies regenerate' to write policy files.");
    Ok(())
}

async fn run_regenerate(
    args: PolicyRegenerateArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas =
        parse_all_schemas_with_global(&[std::path::PathBuf::from("schemas/")], global, output)?;

    // Create output directory
    fs::create_dir_all(&args.output_dir).map_err(|e| CliError::Io {
        path: args.output_dir.clone(),
        source: e,
    })?;

    let mut total_files = 0usize;

    for schema in &schemas {
        if let Some(ref filter) = args.schema {
            if schema.name.as_str() != filter {
                continue;
            }
        }

        let policies = generate_cedar_policies(schema);
        let filename = format!("{}.cedar", schema.name.as_str().to_ascii_lowercase());
        let filepath = args.output_dir.join(&filename);

        if filepath.exists() && !args.force {
            output.warn(&format!(
                "Skipping {} (exists, use --force to overwrite)",
                filepath.display()
            ));
            continue;
        }

        let content: String = policies
            .iter()
            .map(|p| format!("// {}\n{}\n", p.description, p.cedar_text))
            .collect::<Vec<_>>()
            .join("\n");

        fs::write(&filepath, content).map_err(|e| CliError::Io {
            path: filepath.clone(),
            source: e,
        })?;

        total_files += 1;
        output.status(&format!("  Wrote {}", filepath.display()));
    }

    output.success(&format!(
        "Regenerated {total_files} policy files in {}",
        args.output_dir.display()
    ));

    Ok(())
}

/// The schema set the daemon compiles its Cedar bundle from.
#[derive(Debug)]
struct BundleSchemas {
    /// Every schema in the bundle: the built-in system schemas with the
    /// operator's schemas merged over them, in the daemon's order.
    schemas: Vec<SchemaDefinition>,
    /// How many built-in system schemas the bundle kept, i.e. those no
    /// operator schema redefines.
    system_count: usize,
}

/// Parse the built-in system schemas (User, TenantMembership, OAuthIdentity,
/// WebhookSubscription) the daemon registers at startup.
///
/// Mirrors `SchemaForgeExtension::build_init` on a fresh deployment: every
/// text from [`all_system_schemas`] goes through the same DSL parser, and the
/// batch takes the same inverse-relation pairing pass.
fn parse_system_schemas() -> Result<Vec<SchemaDefinition>, CliError> {
    let mut schemas = Vec::new();
    for source in all_system_schemas() {
        let parsed = schema_forge_dsl::parse(source).map_err(|errors| CliError::Parse {
            errors,
            source_text: source.to_string(),
            file: PathBuf::from("built-in system schema"),
        })?;
        schemas.extend(parsed);
    }
    pair_inverse_relations(&mut schemas)
        .map_err(|e| CliError::Other(format!("invalid built-in system schemas: {e}")))?;
    Ok(schemas)
}

/// Build the schema set `serve` compiles its Cedar bundle from.
///
/// The daemon registers the system schemas, then merges the operator's
/// parsed schemas over that registry by name with
/// [`merge_schema_definitions`]. Reusing the same helper keeps the offline
/// bundle identical to the daemon's, including on a name collision, where
/// the operator's definition replaces the built-in one.
fn bundle_schemas(user_schemas: &[SchemaDefinition]) -> Result<BundleSchemas, CliError> {
    let system = parse_system_schemas()?;
    let system_count = system
        .iter()
        .filter(|builtin| user_schemas.iter().all(|user| user.name != builtin.name))
        .count();
    Ok(BundleSchemas {
        schemas: merge_schema_definitions(system, user_schemas),
        system_count,
    })
}

/// Compile the full Cedar bundle (Cedar schema + generated policies +
/// custom policies) and run strict-mode validation. The exit code mirrors
/// the validation result so CI / pre-deploy hooks can gate on a passing
/// bundle.
///
/// The bundle includes the built-in system schemas exactly as the daemon
/// registers them, so custom policies may reference `User`,
/// `TenantMembership`, `OAuthIdentity`, `WebhookSubscription`, and their
/// actions (such as `InviteUser`). Reported schema counts describe the
/// operator's schemas; the human summary also names the system schemas.
async fn run_validate(
    args: PolicyValidateArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let schemas = parse_all_schemas_with_global(&args.schema_paths, global, output)?;

    let role_ranks = RoleRanks::from_toml_file(&args.role_ranks).map_err(|e| {
        CliError::Other(format!(
            "failed to load role ranks from '{}': {e}",
            args.role_ranks.display()
        ))
    })?;

    let bundle = bundle_schemas(&schemas)?;

    let custom_dir = args.custom_dir.as_deref();
    let snapshot = PolicyStoreSnapshot::from_schemas(
        &bundle.schemas,
        custom_dir,
        role_ranks,
        // Lint runs without the operator's runtime config; an empty mapping
        // exercises the same strict-mode pipeline the daemon uses minus the
        // operator-configured principal attributes. Custom policies that
        // reference operator-mapped attributes will surface as validation
        // errors here, which is the conservative bias for a pre-flight lint.
        schema_forge_acton::authz::PrincipalClaimMappings::default(),
    )
    .map_err(|e| CliError::Other(format!("Cedar policy validation failed:\n{e}")))?;

    match output.mode {
        OutputMode::Json => {
            let json = serde_json::json!({
                "ok": true,
                "schema_count": schemas.len(),
                "policy_count": snapshot.policy_count,
                "policy_hash": snapshot.policy_hash,
                "custom_dir": custom_dir.map(|p| p.display().to_string()),
                "role_ranks": args.role_ranks.display().to_string(),
            });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            println!(
                "{}\t{}\t{}",
                schemas.len(),
                snapshot.policy_count,
                snapshot.policy_hash
            );
        }
        OutputMode::Human => {
            output.success(&format!(
                "Cedar bundle validated: {} schemas (+{} system), {} policies, hash {}",
                schemas.len(),
                bundle.system_count,
                snapshot.policy_count,
                &snapshot.policy_hash[..16],
            ));
            if let Some(dir) = custom_dir {
                output.status(&format!("  Custom policies: {}", dir.display()));
            }
            output.status(&format!("  Role ranks: {}", args.role_ranks.display()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(schemas: &[SchemaDefinition]) -> Vec<&str> {
        schemas.iter().map(|schema| schema.name.as_str()).collect()
    }

    #[test]
    fn system_schemas_are_the_four_the_daemon_registers() {
        let system = parse_system_schemas().expect("built-in schemas parse");

        let mut found = names(&system);
        found.sort_unstable();
        assert_eq!(
            found,
            [
                "OAuthIdentity",
                "TenantMembership",
                "User",
                "WebhookSubscription"
            ]
        );
    }

    #[test]
    fn bundle_adds_every_system_schema_beside_the_operator_schemas() {
        let user = schema_forge_dsl::parse("schema Contact { name: text required }").unwrap();

        let bundle = bundle_schemas(&user).unwrap();

        assert_eq!(bundle.system_count, 4);
        assert_eq!(
            names(&bundle.schemas),
            [
                "Contact",
                "OAuthIdentity",
                "TenantMembership",
                "User",
                "WebhookSubscription"
            ]
        );
    }

    #[test]
    fn operator_schema_with_a_system_name_replaces_the_builtin_like_serve() {
        let user = schema_forge_dsl::parse("schema User { nickname: text required }").unwrap();

        let bundle = bundle_schemas(&user).unwrap();

        assert_eq!(bundle.system_count, 3);
        let users: Vec<_> = bundle
            .schemas
            .iter()
            .filter(|schema| schema.name.as_str() == "User")
            .collect();
        assert_eq!(users.len(), 1, "one User definition survives the merge");
        assert_eq!(users[0], &user[0], "the operator's definition wins");
    }
}
