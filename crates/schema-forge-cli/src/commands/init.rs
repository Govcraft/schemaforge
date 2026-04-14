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

    // Generate config.toml with defaults
    create_config_file(&project_dir)?;

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

fn create_config_file(project_dir: &Path) -> Result<(), CliError> {
    let config_content = r#"[database]
url = "ws://localhost:8000"
namespace = "schemaforge"
database = "dev"

[cli]
default_schema_dir = "schemas/"
default_policy_dir = "policies/"
"#;
    write_file(&project_dir.join("config.toml"), config_content)
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
        create_config_file(&project).unwrap();
        let content = std::fs::read_to_string(project.join("config.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert!(parsed.get("database").is_some());
        assert!(parsed.get("cli").is_some());
    }
}
