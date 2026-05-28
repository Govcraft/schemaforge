use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{GlobalOpts, InitArgs};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `init` command: scaffold a new SchemaForge project.
pub async fn run(
    args: InitArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let project_dir = PathBuf::from(&args.name);

    // Check if directory exists
    if project_dir.exists() && !args.force {
        return Err(CliError::DirectoryExists { path: project_dir });
    }

    let template = validate_template(&args.template)?;

    // Create directories and files based on template
    create_project_structure(&project_dir, template)?;

    // Generate config.toml with defaults, branding it with the project name so
    // user-facing flows (invitation emails) read with the application's name
    // out of the box rather than the SchemaForge engine name.
    create_config_file(&project_dir, &args.name)?;

    // Output summary
    match output.mode {
        OutputMode::Human => {
            output.success(&format!(
                "Created project '{}' from '{}' template.",
                args.name, args.template
            ));
            println!();
            print_project_tree(&project_dir, template);
            println!();
            println!("Next steps:");
            println!("  cd {}", args.name);
            println!("  schema-forge parse           Validate schemas");
            println!("  schema-forge generate         Design schemas with AI");
            println!("  schema-forge serve            Start the development server");
        }
        OutputMode::Json => {
            let json = serde_json::json!({
                "project": args.name,
                "template": args.template,
                "path": project_dir.display().to_string(),
            });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            println!(
                "{}\t{}\t{}",
                args.name,
                args.template,
                project_dir.display()
            );
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Template {
    Minimal,
    Full,
    ApiOnly,
}

fn validate_template(name: &str) -> Result<Template, CliError> {
    match name {
        "minimal" => Ok(Template::Minimal),
        "full" => Ok(Template::Full),
        "api-only" => Ok(Template::ApiOnly),
        other => Err(CliError::Config {
            message: format!(
                "unknown template '{other}'. Valid templates: minimal, full, api-only"
            ),
        }),
    }
}

fn create_project_structure(project_dir: &Path, template: Template) -> Result<(), CliError> {
    // Always create schemas directory
    create_dir(project_dir)?;
    create_dir(&project_dir.join("schemas"))?;

    // Create an example schema
    let example_schema = r#"schema Contact {
    name: text(max: 255) required
    email: text required indexed
    phone: text
    active: boolean default(true)
}
"#;
    write_file(&project_dir.join("schemas/example.schema"), example_schema)?;

    match template {
        Template::Minimal => {
            // Just schemas/ and config.toml (config created separately)
        }
        Template::ApiOnly => {
            create_dir(&project_dir.join("policies"))?;
            create_dir(&project_dir.join("policies/generated"))?;
            create_dir(&project_dir.join("policies/custom"))?;
        }
        Template::Full => {
            create_dir(&project_dir.join("policies"))?;
            create_dir(&project_dir.join("policies/generated"))?;
            create_dir(&project_dir.join("policies/custom"))?;

            // Placeholder Dockerfile
            let dockerfile =
                "# SchemaForge Application\nFROM rust:1.84 as builder\n# TODO: customize\n";
            write_file(&project_dir.join("Dockerfile"), dockerfile)?;

            // K8s directory
            create_dir(&project_dir.join("k8s"))?;
            write_file(&project_dir.join("k8s/.gitkeep"), "")?;
        }
    }

    Ok(())
}

/// Escape a string for use inside a TOML basic (double-quoted) string.
///
/// A project name is operator-supplied and becomes the directory name, so it
/// can contain characters (`"`, `\`) that would break the generated TOML if
/// interpolated raw. Escape exactly what a basic string requires.
fn toml_escape_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn create_config_file(project_dir: &Path, project_name: &str) -> Result<(), CliError> {
    // Schema-forge uses acton-service's canonical config layout: SurrealDB
    // settings live under [surrealdb], PostgreSQL settings under [database].
    // CLI flags (`--db-url`, `--db-ns`, `--db-name`) and `ACTON_*` env vars
    // override these in-place; there is no parallel schema-forge config layer.
    let project_name_block = format!(
        "[schema_forge]\n\
         # Human-facing name of this deployment. Shown to users in invitation\n\
         # emails and used as the default email From display-name. Edit this to\n\
         # the name users should recognize. Defaults to \"SchemaForge\" if removed.\n\
         project_name = \"{}\"\n\n",
        toml_escape_basic(project_name)
    );
    let config_content = format!(
        "{project_name_block}{}",
        r#"[surrealdb]
url = "ws://localhost:8000"
namespace = "schemaforge"
database = "dev"

# Switch to PostgreSQL by removing [surrealdb] above and uncommenting:
# [database]
# url = "postgres://user:pass@localhost:5432/schemaforge"

# ---------------------------------------------------------------------------
# Signed-schema enforcement.
#
# Fresh projects default to `mode = "off"` (the section is commented
# out below) so `schemaforge apply` works against an unsigned schema
# directory on day one. The recommended rollout is three stages:
#
#   1. off (current, while you finish writing schemas).
#   2. warn — uncomment the block below as written. Every command
#      runs the full verifier; failures are logged but do not abort.
#      Use this to test signing in dev and CI without breaking the
#      build, typically for one release cycle.
#   3. enforce — change `mode = "warn"` to `mode = "enforce"` once
#      every schema is signed and CI is green. Any verification
#      failure aborts the command with exit code 13.
#
# Three signing schemes are shipped — pick whichever matches the keys
# you already manage:
#
# Option A — raw ed25519 (generate or reuse a PEM key):
#
#   schemaforge sign schemas/ --ed25519-generate ./keys/sign.pem --print-pubkey
#
# Option B — sign with an existing OpenSSH key (`~/.ssh/id_ed25519` etc.):
#
#   schemaforge sign schemas/ --ssh-key ~/.ssh/id_ed25519 --print-pubkey
#
# Option C — Sigstore cosign keyless (workload OIDC, typically GitHub
# Actions). Requires the `cosign` binary on the runner:
#
#   schemaforge sign schemas/ --keyless
#
# Paste the printed trust block into `[[schema_forge.signing.
# trusted_signers]]` below before flipping the mode away from off.
# ---------------------------------------------------------------------------
# [schema_forge.signing]
# mode = "warn"      # off | warn | enforce — warn is the migration stop.
#
# # Ed25519 anchor (Option A):
# [[schema_forge.signing.trusted_signers]]
# kind = "ed25519"
# name = "ops-key"
# public_key_b64 = "PASTE-SPKI-BASE64-FROM-schemaforge-sign-OUTPUT"
#
# # SSH allowed_signers anchor (Option B):
# [[schema_forge.signing.trusted_signers]]
# kind = "ssh-allowed-signers"
# name = "ops-allowed-signers"
# path = "/etc/schemaforge/allowed_signers"
#
# # Cosign-keyless anchor (Option C). `issuer` is the exact OIDC
# # issuer string the workload's token vendor sets; `subject_pattern`
# # is a glob matched against the certificate's OIDC subject (SAN).
# # For airgap deployments, set `trust_root_bundle` to a refreshed
# # Sigstore TUF snapshot (see `schemaforge trust-bundle refresh`):
# #   trust_root_bundle = "/etc/schemaforge/trust_root.json"
# [[schema_forge.signing.trusted_signers]]
# kind = "cosign-keyless"
# name = "release-pipeline"
# issuer = "https://token.actions.githubusercontent.com"
# subject_pattern = "https://github.com/<org>/<repo>/.github/workflows/release.yml@refs/tags/v*"
"#
    );
    write_file(&project_dir.join("config.toml"), &config_content)
}

fn create_dir(path: &Path) -> Result<(), CliError> {
    fs::create_dir_all(path).map_err(|e| CliError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), CliError> {
    fs::write(path, content).map_err(|e| CliError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

fn print_project_tree(project_dir: &Path, template: Template) {
    let name = project_dir.display();
    println!("  {name}/");
    println!("    config.toml");
    println!("    schemas/");
    println!("      example.schema");

    match template {
        Template::Minimal => {}
        Template::ApiOnly => {
            println!("    policies/");
            println!("      generated/");
            println!("      custom/");
        }
        Template::Full => {
            println!("    policies/");
            println!("      generated/");
            println!("      custom/");
            println!("    Dockerfile");
            println!("    k8s/");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_template_minimal() {
        assert_eq!(validate_template("minimal").unwrap(), Template::Minimal);
    }

    #[test]
    fn validate_template_full() {
        assert_eq!(validate_template("full").unwrap(), Template::Full);
    }

    #[test]
    fn validate_template_api_only() {
        assert_eq!(validate_template("api-only").unwrap(), Template::ApiOnly);
    }

    #[test]
    fn validate_template_invalid() {
        assert!(validate_template("bad").is_err());
    }

    #[test]
    fn create_minimal_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("test-project");
        create_project_structure(&project, Template::Minimal).unwrap();
        assert!(project.join("schemas").exists());
        assert!(project.join("schemas/example.schema").exists());
    }

    #[test]
    fn create_full_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("test-project");
        create_project_structure(&project, Template::Full).unwrap();
        assert!(project.join("schemas").exists());
        assert!(project.join("policies/generated").exists());
        assert!(project.join("policies/custom").exists());
        assert!(project.join("Dockerfile").exists());
        assert!(project.join("k8s").exists());
    }

    #[test]
    fn create_api_only_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("test-project");
        create_project_structure(&project, Template::ApiOnly).unwrap();
        assert!(project.join("schemas").exists());
        assert!(project.join("policies/generated").exists());
        assert!(project.join("policies/custom").exists());
        assert!(!project.join("Dockerfile").exists());
    }

    #[test]
    fn create_config_file_creates_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("test-project");
        std::fs::create_dir_all(&project).unwrap();
        create_config_file(&project, "test-project").unwrap();
        let content = std::fs::read_to_string(project.join("config.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        // Template uses acton-service's canonical layout: SurrealDB lives
        // under [surrealdb], Postgres under [database]. The default
        // template wires up a dev SurrealDB; [database] is shown as a
        // commented switch.
        assert!(
            parsed.get("surrealdb").is_some(),
            "default config.toml must contain [surrealdb]"
        );
        assert!(
            parsed.get("database").is_none(),
            "default config.toml must not enable [database] (postgres is opt-in)"
        );
    }

    #[test]
    fn create_config_file_brands_project_name() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("bobs-dogs");
        std::fs::create_dir_all(&project).unwrap();
        create_config_file(&project, "Bob's Dog Scheduling").unwrap();
        let content = std::fs::read_to_string(project.join("config.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed
                .get("schema_forge")
                .and_then(|v| v.get("project_name"))
                .and_then(|v| v.as_str()),
            Some("Bob's Dog Scheduling"),
            "scaffold must seed [schema_forge] project_name from the init name"
        );
    }

    #[test]
    fn create_config_file_escapes_project_name_for_toml() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("quoted");
        std::fs::create_dir_all(&project).unwrap();
        // A name with a double-quote and backslash must not break the TOML.
        create_config_file(&project, r#"Ann "Q" \ Co"#).unwrap();
        let content = std::fs::read_to_string(project.join("config.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed
                .get("schema_forge")
                .and_then(|v| v.get("project_name"))
                .and_then(|v| v.as_str()),
            Some(r#"Ann "Q" \ Co"#)
        );
    }

    /// The signing scaffold is shipped fully commented out — fresh
    /// projects parse as `mode = "off"` by virtue of the section not
    /// existing. This guards against accidental uncommenting (which
    /// would force every new project to configure trust anchors before
    /// `apply` runs).
    #[test]
    fn create_config_file_leaves_signing_block_commented_out() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("test-project");
        std::fs::create_dir_all(&project).unwrap();
        create_config_file(&project, "test-project").unwrap();
        let content = std::fs::read_to_string(project.join("config.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        // [schema_forge] now exists (it carries project_name), but the
        // signing subtable must stay commented so fresh projects parse as
        // `mode = "off"` and `apply` works without trust anchors on day one.
        assert!(
            parsed
                .get("schema_forge")
                .and_then(|v| v.get("signing"))
                .is_none(),
            "scaffold must NOT activate [schema_forge.signing] by default; \
             it ships commented so fresh projects start in `mode = \"off\"`",
        );
        // The recommended migration stop is "warn", not "enforce" —
        // operators uncommenting the block should land on warn and
        // promote when ready. Guard that the example line in the
        // scaffold still says so.
        assert!(
            content.contains("# mode = \"warn\""),
            "scaffold should recommend `mode = \"warn\"` as the migration stop",
        );
    }

    /// Round-trip drill: extract the commented signing block from
    /// the scaffold, strip the comment prefix, and feed it back into
    /// the trust-policy parser. The TOML committed into the scaffold
    /// must always parse cleanly — bit-rot in the example would
    /// leave operators copying a broken config when they uncomment.
    #[test]
    fn commented_signing_block_in_scaffold_parses_when_uncommented() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("test-project");
        std::fs::create_dir_all(&project).unwrap();
        create_config_file(&project, "test-project").unwrap();
        let content = std::fs::read_to_string(project.join("config.toml")).unwrap();

        // Pull every line in the [schema_forge.signing] commented
        // example and uncomment it. The block starts at the
        // `# [schema_forge.signing]` line and extends to EOF in the
        // scaffold.
        let signing_block: String = content
            .lines()
            .skip_while(|line| !line.trim_start().starts_with("# [schema_forge.signing]"))
            .map(|line| {
                line.strip_prefix("# ")
                    .or_else(|| line.strip_prefix("#"))
                    .unwrap_or(line)
                    .to_string()
                    + "\n"
            })
            .collect();

        // Stub out the placeholder so the example parses as a real
        // base64 string. We're testing TOML structure, not key
        // material.
        let materialised = signing_block.replace(
            "PASTE-SPKI-BASE64-FROM-schemaforge-sign-OUTPUT",
            "AAAA",
        );

        let parsed: toml::Value = toml::from_str(&materialised).unwrap_or_else(|e| {
            panic!("scaffold's signing example failed to parse:\n{materialised}\nerror: {e}")
        });
        let signing = parsed
            .get("schema_forge")
            .and_then(|v| v.get("signing"))
            .expect("expected [schema_forge.signing] table");
        let mode = signing.get("mode").and_then(|v| v.as_str()).unwrap();
        assert_eq!(mode, "warn", "scaffold default starting mode must be warn");
        let signers = signing
            .get("trusted_signers")
            .and_then(|v| v.as_array())
            .expect("expected [[trusted_signers]] array");
        assert!(
            signers.len() >= 3,
            "scaffold should illustrate all three trust-anchor kinds (ed25519, ssh, cosign)",
        );
    }
}
