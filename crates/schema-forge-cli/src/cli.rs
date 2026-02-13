use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

/// Adaptive Object Model runtime with a human-readable DSL.
///
/// SchemaForge turns plain English descriptions into fully operational,
/// enterprise-grade backends. Define schemas in a human-readable DSL,
/// generate them with AI, and deploy with zero recompilation.
#[derive(Parser)]
#[command(
    name = "schema-forge",
    version,
    about = "Adaptive Object Model runtime with a human-readable DSL",
    after_help = "Use 'schema-forge <command> --help' for more information about a command.\n\
                  Documentation: https://github.com/GovCraft/schema-forge",
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[command(flatten)]
    pub global: GlobalOpts,
}

/// Global options available to all subcommands.
#[derive(Args, Debug)]
pub struct GlobalOpts {
    /// Configuration file path [env: SCHEMA_FORGE_CONFIG]
    #[arg(
        short = 'c',
        long = "config",
        global = true,
        env = "SCHEMA_FORGE_CONFIG"
    )]
    pub config: Option<PathBuf>,

    /// Output format: human (default), json, plain
    #[arg(
        long,
        global = true,
        default_value = "human",
        value_parser = ["human", "json", "plain"]
    )]
    pub format: String,

    /// Increase verbosity (-v, -vv, -vvv)
    #[arg(short = 'v', long = "verbose", global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Suppress all non-error output
    #[arg(short = 'q', long = "quiet", global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Disable colored output [env: NO_COLOR]
    #[arg(long = "no-color", global = true, env = "NO_COLOR")]
    pub no_color: bool,

    /// SurrealDB connection URL [env: SCHEMA_FORGE_DB_URL]
    #[arg(long = "db-url", global = true, env = "SCHEMA_FORGE_DB_URL")]
    pub db_url: Option<String>,

    /// SurrealDB namespace [env: SCHEMA_FORGE_DB_NS]
    #[arg(long = "db-ns", global = true, env = "SCHEMA_FORGE_DB_NS")]
    pub db_ns: Option<String>,

    /// SurrealDB database name [env: SCHEMA_FORGE_DB_NAME]
    #[arg(long = "db-name", global = true, env = "SCHEMA_FORGE_DB_NAME")]
    pub db_name: Option<String>,
}

/// Top-level subcommands.
#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new SchemaForge project
    Init(InitArgs),

    /// Parse and validate .schema files
    Parse(ParseArgs),

    /// Apply .schema files to a running backend
    Apply(ApplyArgs),

    /// Plan and execute schema migrations
    Migrate(MigrateArgs),

    /// Generate schemas from natural language descriptions
    Generate(GenerateArgs),

    /// Start acton-service with SchemaForge extension
    Serve(ServeArgs),

    /// Export schemas in various formats
    Export {
        #[command(subcommand)]
        command: ExportCommands,
    },

    /// Inspect registered schemas, entity counts, and indexes
    Inspect(InspectArgs),

    /// Manage Cedar authorization policies
    Policies {
        #[command(subcommand)]
        command: PolicyCommands,
    },

    /// Generate shell completion scripts
    Completions(CompletionsArgs),
}

// ---------------------------------------------------------------------------
// Individual command argument structs
// ---------------------------------------------------------------------------

/// Arguments for `schema-forge init`.
#[derive(Args)]
pub struct InitArgs {
    /// Project name (becomes directory name)
    pub name: String,

    /// Project template: minimal, full, api-only
    #[arg(short = 't', long = "template", default_value = "full")]
    pub template: String,

    /// Force creation even if directory exists
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Skip interactive prompts, use defaults
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

/// Arguments for `schema-forge parse`.
#[derive(Args)]
pub struct ParseArgs {
    /// Schema files or directories to parse (default: ./schemas/)
    #[arg(default_value = "schemas/")]
    pub paths: Vec<PathBuf>,

    /// Show the parsed AST as DSL (round-trip output)
    #[arg(long = "print")]
    pub print_ast: bool,

    /// Show detailed token-level parse information
    #[arg(short = 'd', long = "debug")]
    pub debug: bool,
}

/// Arguments for `schema-forge apply`.
#[derive(Args)]
pub struct ApplyArgs {
    /// Schema files or directories to apply (default: ./schemas/)
    #[arg(default_value = "schemas/")]
    pub paths: Vec<PathBuf>,

    /// Dry-run: show what would be applied without executing
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,

    /// Force apply even for destructive changes
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Auto-generate Cedar policies for new schemas
    #[arg(long = "with-policies")]
    pub with_policies: bool,
}

/// Arguments for `schema-forge migrate`.
#[derive(Args)]
pub struct MigrateArgs {
    /// Schema files or directories (default: ./schemas/)
    #[arg(default_value = "schemas/")]
    pub paths: Vec<PathBuf>,

    /// Execute the migration plan (default is dry-run)
    #[arg(long = "execute")]
    pub execute: bool,

    /// Force apply destructive steps without confirmation
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Show only a specific schema's migration
    #[arg(short = 's', long = "schema")]
    pub schema: Option<String>,
}

/// Arguments for `schema-forge generate`.
#[derive(Args)]
pub struct GenerateArgs {
    /// Natural language description (if omitted, enters interactive mode)
    pub description: Option<String>,

    /// Output file for generated schema (default: stdout)
    #[arg(short = 'o', long = "output")]
    pub output: Option<PathBuf>,

    /// AI provider: auto (from config), ollama, anthropic, openai
    #[arg(long = "provider", default_value = "auto")]
    pub provider: String,

    /// Model name (provider-specific)
    #[arg(long = "model")]
    pub model: Option<String>,

    /// Non-interactive: generate from description, write to output, exit
    #[arg(long = "batch")]
    pub batch: bool,
}

/// Arguments for `schema-forge serve`.
#[derive(Args)]
pub struct ServeArgs {
    /// Host address to bind
    #[arg(short = 'H', long = "host", default_value = "127.0.0.1")]
    pub host: String,

    /// Port to listen on
    #[arg(short = 'p', long = "port", default_value = "3000")]
    pub port: u16,

    /// Schema files to load on startup
    #[arg(long = "schemas", default_value = "schemas/")]
    pub schema_dir: PathBuf,

    /// Watch schema files for changes and hot-reload
    #[arg(short = 'w', long = "watch")]
    pub watch: bool,

    /// Log level override (trace, debug, info, warn, error)
    #[arg(long = "log-level")]
    pub log_level: Option<String>,

    /// Admin username for the admin/cloud UI (bootstraps on first run)
    #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
    #[arg(long = "admin-user", env = "FORGE_ADMIN_USER", default_value = "admin")]
    pub admin_user: String,

    /// Admin password for the admin/cloud UI (bootstraps on first run)
    #[cfg(any(feature = "admin-ui", feature = "cloud-ui"))]
    #[arg(long = "admin-password", env = "FORGE_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,
}

/// Export subcommands.
#[derive(Subcommand)]
pub enum ExportCommands {
    /// Export OpenAPI specification
    Openapi(ExportOpenapiArgs),
}

/// Arguments for `schema-forge export openapi`.
#[derive(Args)]
pub struct ExportOpenapiArgs {
    /// Output file (default: stdout)
    #[arg(short = 'o', long = "output")]
    pub output: Option<PathBuf>,

    /// Schema files to include (default: ./schemas/)
    #[arg(default_value = "schemas/")]
    pub paths: Vec<PathBuf>,

    /// API base path prefix
    #[arg(long = "base-path", default_value = "/forge")]
    pub base_path: String,

    /// OpenAPI spec version
    #[arg(long = "spec-version", default_value = "3.1.0")]
    pub spec_version: String,
}

/// Arguments for `schema-forge inspect`.
#[derive(Args)]
pub struct InspectArgs {
    /// Show a specific schema (omit for all)
    pub schema: Option<String>,

    /// Show detailed field information
    #[arg(short = 'd', long = "detail")]
    pub detail: bool,

    /// Include entity count per schema (requires backend query)
    #[arg(long = "counts")]
    pub counts: bool,
}

/// Policy subcommands.
#[derive(Subcommand)]
pub enum PolicyCommands {
    /// List generated Cedar policies for all or specific schemas
    List(PolicyListArgs),

    /// Regenerate Cedar policy templates
    Regenerate(PolicyRegenerateArgs),
}

/// Arguments for `schema-forge policies list`.
#[derive(Args)]
pub struct PolicyListArgs {
    /// Show policies for a specific schema
    pub schema: Option<String>,
}

/// Arguments for `schema-forge policies regenerate`.
#[derive(Args)]
pub struct PolicyRegenerateArgs {
    /// Schema to regenerate policies for (omit for all)
    pub schema: Option<String>,

    /// Output directory for generated policies
    #[arg(short = 'o', long = "output", default_value = "policies/generated/")]
    pub output_dir: PathBuf,

    /// Overwrite existing generated policies
    #[arg(short = 'f', long = "force")]
    pub force: bool,
}

/// Arguments for `schema-forge completions`.
#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_parser = ["bash", "zsh", "fish", "powershell", "elvish"])]
    pub shell: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli_structure() {
        // This validates the derive macros produce a valid clap command.
        Cli::command().debug_assert();
    }

    #[test]
    fn parse_minimal_args() {
        let cli = Cli::try_parse_from(["schema-forge", "completions", "bash"]).unwrap();
        assert!(matches!(cli.command, Commands::Completions(_)));
    }

    #[test]
    fn parse_global_verbose() {
        let cli = Cli::try_parse_from(["schema-forge", "-vvv", "completions", "bash"]).unwrap();
        assert_eq!(cli.global.verbose, 3);
    }

    #[test]
    fn parse_global_quiet() {
        let cli = Cli::try_parse_from(["schema-forge", "-q", "completions", "bash"]).unwrap();
        assert!(cli.global.quiet);
    }

    #[test]
    fn parse_global_format_json() {
        let cli = Cli::try_parse_from(["schema-forge", "--format", "json", "completions", "bash"])
            .unwrap();
        assert_eq!(cli.global.format, "json");
    }

    #[test]
    fn parse_init_command() {
        let cli =
            Cli::try_parse_from(["schema-forge", "init", "my-project", "-t", "minimal"]).unwrap();
        if let Commands::Init(args) = cli.command {
            assert_eq!(args.name, "my-project");
            assert_eq!(args.template, "minimal");
            assert!(!args.force);
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn parse_parse_command_with_print() {
        let cli = Cli::try_parse_from(["schema-forge", "parse", "--print", "schemas/"]).unwrap();
        if let Commands::Parse(args) = cli.command {
            assert!(args.print_ast);
            assert_eq!(args.paths, vec![PathBuf::from("schemas/")]);
        } else {
            panic!("expected Parse command");
        }
    }

    #[test]
    fn parse_apply_command_dry_run() {
        let cli = Cli::try_parse_from(["schema-forge", "apply", "--dry-run"]).unwrap();
        if let Commands::Apply(args) = cli.command {
            assert!(args.dry_run);
            assert!(!args.force);
        } else {
            panic!("expected Apply command");
        }
    }

    #[test]
    fn parse_migrate_command() {
        let cli = Cli::try_parse_from([
            "schema-forge",
            "migrate",
            "--execute",
            "--schema",
            "Contact",
        ])
        .unwrap();
        if let Commands::Migrate(args) = cli.command {
            assert!(args.execute);
            assert_eq!(args.schema, Some("Contact".to_string()));
        } else {
            panic!("expected Migrate command");
        }
    }

    #[test]
    fn parse_inspect_command() {
        let cli = Cli::try_parse_from(["schema-forge", "inspect", "Contact", "--detail"]).unwrap();
        if let Commands::Inspect(args) = cli.command {
            assert_eq!(args.schema, Some("Contact".to_string()));
            assert!(args.detail);
        } else {
            panic!("expected Inspect command");
        }
    }

    #[test]
    fn parse_export_openapi() {
        let cli =
            Cli::try_parse_from(["schema-forge", "export", "openapi", "-o", "api.json"]).unwrap();
        if let Commands::Export {
            command: ExportCommands::Openapi(args),
        } = cli.command
        {
            assert_eq!(args.output, Some(PathBuf::from("api.json")));
        } else {
            panic!("expected Export Openapi command");
        }
    }

    #[test]
    fn parse_policies_list() {
        let cli = Cli::try_parse_from(["schema-forge", "policies", "list", "Contact"]).unwrap();
        if let Commands::Policies {
            command: PolicyCommands::List(args),
        } = cli.command
        {
            assert_eq!(args.schema, Some("Contact".to_string()));
        } else {
            panic!("expected Policies List command");
        }
    }

    #[test]
    fn parse_policies_regenerate() {
        let cli = Cli::try_parse_from([
            "schema-forge",
            "policies",
            "regenerate",
            "--output",
            "/tmp/policies",
            "--force",
        ])
        .unwrap();
        if let Commands::Policies {
            command: PolicyCommands::Regenerate(args),
        } = cli.command
        {
            assert_eq!(args.output_dir, PathBuf::from("/tmp/policies"));
            assert!(args.force);
        } else {
            panic!("expected Policies Regenerate command");
        }
    }

    #[test]
    fn parse_serve_command() {
        let cli = Cli::try_parse_from([
            "schema-forge",
            "serve",
            "-H",
            "0.0.0.0",
            "-p",
            "8080",
            "--watch",
        ])
        .unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.host, "0.0.0.0");
            assert_eq!(args.port, 8080);
            assert!(args.watch);
        } else {
            panic!("expected Serve command");
        }
    }

    #[test]
    fn parse_generate_command() {
        let cli = Cli::try_parse_from([
            "schema-forge",
            "generate",
            "A CRM with contacts",
            "--batch",
            "-o",
            "crm.schema",
        ])
        .unwrap();
        if let Commands::Generate(args) = cli.command {
            assert_eq!(args.description, Some("A CRM with contacts".to_string()));
            assert!(args.batch);
            assert_eq!(args.output, Some(PathBuf::from("crm.schema")));
        } else {
            panic!("expected Generate command");
        }
    }

    #[test]
    fn verbose_and_quiet_conflict() {
        let result = Cli::try_parse_from(["schema-forge", "-v", "-q", "completions", "bash"]);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_format_rejected() {
        let result =
            Cli::try_parse_from(["schema-forge", "--format", "xml", "completions", "bash"]);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_shell_rejected() {
        let result = Cli::try_parse_from(["schema-forge", "completions", "tcsh"]);
        assert!(result.is_err());
    }
}
