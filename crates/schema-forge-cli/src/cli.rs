use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

/// Adaptive Object Model runtime with a human-readable DSL.
///
/// SchemaForge lets you define schemas in a human-readable DSL
/// and deploy with zero recompilation.
#[derive(Parser)]
#[command(
    name = "schemaforge",
    version,
    about = "Adaptive Object Model runtime with a human-readable DSL",
    after_help = "Use 'schemaforge <command> --help' for more information about a command.\n\
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

    /// Database connection URL (auto-detects backend from scheme) [env: SCHEMA_FORGE_DB_URL]
    #[arg(long = "db-url", global = true, env = "SCHEMA_FORGE_DB_URL")]
    pub db_url: Option<String>,

    /// Database namespace (SurrealDB only) [env: SCHEMA_FORGE_DB_NS]
    #[arg(long = "db-ns", global = true, env = "SCHEMA_FORGE_DB_NS")]
    pub db_ns: Option<String>,

    /// Database name (SurrealDB only) [env: SCHEMA_FORGE_DB_NAME]
    #[arg(long = "db-name", global = true, env = "SCHEMA_FORGE_DB_NAME")]
    pub db_name: Option<String>,

    /// Path to a standalone trust-policy TOML file. Overrides the
    /// `[schema_forge.signing]` section of the main config. Useful
    /// when one config.toml is shared across environments but the
    /// trust roots differ per deployment.
    #[arg(
        long = "trust-policy",
        global = true,
        env = "SCHEMAFORGE_TRUST_POLICY"
    )]
    pub trust_policy: Option<PathBuf>,

    /// Skip schema signature verification for this invocation. Refused
    /// when `signing.mode = "enforce"` unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1`
    /// is set — production environments should not accept an
    /// unsigned-schema run silently.
    #[arg(long = "no-verify", global = true)]
    pub no_verify: bool,
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

    /// Manage PASETO tokens (generate keys, mint tokens)
    Token {
        #[command(subcommand)]
        command: TokenCommands,
    },

    /// Call entity REST endpoints on a running instance over HTTP.
    ///
    /// Unlike `inspect`/`apply`/`migrate` — which connect directly to the
    /// database via `--db-url` — this group speaks the REST API against a
    /// deployed server and authenticates with a Bearer PASETO token. Get a
    /// token with `schemaforge login` (credential flow against the server)
    /// or `schemaforge token generate` (offline, from the PASETO key).
    Entity {
        #[command(subcommand)]
        command: EntityCommands,
    },

    /// Authenticate against a running instance and cache a Bearer token.
    ///
    /// Calls `POST /auth/login`, prompts for the password (or reads
    /// `--password-stdin`), and by default writes the returned token to the
    /// XDG state directory so subsequent `entity` commands pick it up
    /// automatically.
    Login(LoginArgs),

    /// Generate shell completion scripts
    Completions(CompletionsArgs),

    /// Generate, list, or diff hook service scaffolds for `@hook(...)`
    /// annotations declared in your schemas.
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },

    /// Generate a React site scaffold (v0: one entity deep).
    Site {
        #[command(subcommand)]
        command: SiteCommands,
    },

    /// Bootstrap the initial `platform_admin` user against the configured
    /// backend. Idempotent: only seeds when the user store is empty.
    BootstrapAdmin(BootstrapAdminArgs),

    /// Sign `.schema` files and produce the directory manifest + per-file
    /// signatures that `schemaforge apply` will later verify.
    Sign(SignArgs),

    /// Verify a `.schema` directory against the trust policy without
    /// applying anything. Suitable as a pre-merge CI gate.
    Verify(VerifyArgs),

    /// Manage the Sigstore TUF trust-root snapshot used by the
    /// `cosign-keyless` verifier. Run `refresh` on a connected host to
    /// fetch the latest material, then copy the JSON into an airgapped
    /// deployment and point `[schema_forge.signing] trust_root_bundle`
    /// at it.
    TrustBundle {
        #[command(subcommand)]
        command: TrustBundleCommands,
    },
}

/// Arguments for `schemaforge sign`.
///
/// Produces a `<dir>/schemas.manifest.toml` (plus its `.sig`) and a
/// `<file>.sig` next to each `.schema`. Existing signature files are
/// overwritten — re-running `sign` after editing a schema is the
/// supported way to refresh signatures.
#[derive(Args)]
pub struct SignArgs {
    /// Directory or individual `.schema` files to sign. Defaults to
    /// `./schemas/`.
    #[arg(default_value = "schemas/")]
    pub paths: Vec<PathBuf>,

    /// Signing scheme.
    #[command(flatten)]
    pub key: SignKeyArgs,

    /// Override the manifest filename. Must match the operator's
    /// `[schema_forge.signing] manifest_filename` if they've changed it.
    #[arg(long = "manifest-filename")]
    pub manifest_filename: Option<String>,

    /// Also print the public key (base64 SPKI) so the operator can
    /// paste it directly into their trust policy.
    #[arg(long = "print-pubkey")]
    pub print_pubkey: bool,

    /// Allow `--ed25519-generate` to overwrite an existing file.
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Principal label used in the `--print-pubkey` allowed-signers
    /// suggestion when signing with `--ssh-key`. Defaults to the key's
    /// OpenSSH comment field (typically `user@host`). Has no effect for
    /// non-SSH signing schemes.
    #[arg(long = "ssh-principal")]
    pub ssh_principal: Option<String>,

    /// Override the `cosign` binary used by `--keyless`. Defaults to
    /// the name `cosign` resolved via `PATH`. Useful when the runner
    /// has multiple cosign versions or installs the binary at a
    /// non-standard location.
    #[arg(long = "cosign-bin", default_value = "cosign")]
    pub cosign_bin: String,
}

/// Selects which signing scheme `schemaforge sign` uses. Exactly one
/// flag must be set.
#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
pub struct SignKeyArgs {
    /// Path to a PKCS#8 PEM-encoded ed25519 private key. Generate one
    /// with `openssl genpkey -algorithm ed25519` or rely on
    /// `--ed25519-generate` to create one in place.
    #[arg(long = "ed25519-key")]
    pub ed25519_key: Option<PathBuf>,

    /// Generate a fresh ed25519 keypair, write the private key to the
    /// given path (PKCS#8 PEM), and use it to sign. Refuses to
    /// overwrite an existing file unless `--force` is also set.
    #[arg(long = "ed25519-generate")]
    pub ed25519_generate: Option<PathBuf>,

    /// Path to an OpenSSH-format private key (e.g. `~/.ssh/id_ed25519`).
    /// Encrypted keys are rejected — decrypt to a temporary path before
    /// passing this flag, or use a key agent if one becomes supported.
    /// SchemaForge signs under the namespace
    /// `schema-forge-signing@govcraft.ai`; the corresponding allowed-
    /// signers entry must either omit the `namespaces=` option or
    /// include that string.
    #[arg(long = "ssh-key")]
    pub ssh_key: Option<PathBuf>,

    /// Use Sigstore cosign keyless signing. Shells out to `cosign
    /// sign-blob --bundle …` once per schema file (and once for the
    /// manifest), so the runtime needs `cosign` in `PATH` and an OIDC
    /// token in the environment (`ACTIONS_ID_TOKEN_REQUEST_TOKEN` /
    /// `ACTIONS_ID_TOKEN_REQUEST_URL` for GitHub Actions, or any flow
    /// supported by `cosign` directly). The on-disk `.sig` artefact is
    /// a Sigstore Bundle JSON (`mediaType
    /// application/vnd.dev.sigstore.bundle.v0.3+json`) — one self-
    /// contained file per signature.
    #[arg(long = "keyless")]
    pub keyless: bool,
}

/// Subcommands for `schemaforge trust-bundle`.
#[derive(Subcommand)]
pub enum TrustBundleCommands {
    /// Fetch the Sigstore production TUF trust-root snapshot and write
    /// it to disk. Operators run this on a connected host, then copy
    /// the JSON across the airgap to a SCIF deployment.
    Refresh(TrustBundleRefreshArgs),

    /// Show a one-line summary of a trust-root JSON file: source
    /// instance, fulcio cert count, rekor key count, TSA cert count.
    /// Useful as a sanity check after `refresh`.
    Inspect(TrustBundleInspectArgs),
}

/// Arguments for `schemaforge trust-bundle refresh`.
#[derive(Args)]
pub struct TrustBundleRefreshArgs {
    /// Output path for the refreshed trust-root JSON. Defaults to
    /// `./trust_root.json` in the current directory.
    #[arg(long = "output", short = 'o', default_value = "trust_root.json")]
    pub output: PathBuf,

    /// Sigstore instance to fetch from. `public-good` is the live
    /// production TUF root that signs releases, attestations, and the
    /// SchemaForge artefacts produced by `cosign sign-blob`. `staging`
    /// fetches the Sigstore staging environment (only useful when
    /// validating new tooling). `github` fetches the GitHub
    /// artifact-attestation trust root.
    #[arg(
        long = "instance",
        default_value = "public-good",
        value_parser = ["public-good", "staging", "github"],
    )]
    pub instance: String,

    /// Allow `refresh` to overwrite an existing file at the output
    /// path. Without this, refusing to clobber is the safer default —
    /// a botched copy across the airgap is worse than a refresh that
    /// errored.
    #[arg(short = 'f', long = "force")]
    pub force: bool,
}

/// Arguments for `schemaforge trust-bundle inspect`.
#[derive(Args)]
pub struct TrustBundleInspectArgs {
    /// Path to a trust-root JSON file produced by `trust-bundle refresh`
    /// (or any compatible Sigstore trust-root snapshot).
    pub path: PathBuf,
}

/// Arguments for `schemaforge verify`.
#[derive(Args)]
pub struct VerifyArgs {
    /// Directory or individual `.schema` files to verify. Defaults to
    /// `./schemas/`. Verification uses the trust policy resolved by
    /// the global `--trust-policy` / `[schema_forge.signing]` rules.
    #[arg(default_value = "schemas/")]
    pub paths: Vec<PathBuf>,
}

/// Arguments for `schema-forge bootstrap-admin`.
///
/// Connects to the configured backend (resolved via the same precedence
/// chain as `serve`: CLI flag → env → config.toml) and creates a user
/// holding the `platform_admin` role. Refuses to run when other users
/// already exist so production deployments don't accidentally double-seed.
#[derive(Args)]
pub struct BootstrapAdminArgs {
    /// Username for the bootstrapped platform_admin.
    #[arg(
        long = "username",
        env = "SCHEMA_FORGE_BOOTSTRAP_ADMIN_USERNAME",
        default_value = "admin"
    )]
    pub username: String,

    /// Password for the bootstrapped platform_admin. Required either as a
    /// flag or via `SCHEMA_FORGE_BOOTSTRAP_ADMIN_PASSWORD`. Never prompted
    /// interactively — operators run this command from provisioning
    /// pipelines (init containers, ansible playbooks) where stdin isn't
    /// available.
    #[arg(long = "password", env = "SCHEMA_FORGE_BOOTSTRAP_ADMIN_PASSWORD")]
    pub password: String,

    /// Display name written into the user record. Cosmetic only.
    #[arg(
        long = "display-name",
        env = "SCHEMA_FORGE_BOOTSTRAP_ADMIN_DISPLAY_NAME",
        default_value = "Administrator"
    )]
    pub display_name: String,

    /// Path to `role_ranks.toml` so the bootstrap user's `role_rank` is
    /// computed against the same hierarchy `serve` will use. Missing file
    /// is treated as the empty hierarchy.
    #[arg(long = "role-ranks", default_value = "policies/role_ranks.toml")]
    pub role_ranks: PathBuf,
}

/// Site generator subcommands.
#[derive(Subcommand)]
pub enum SiteCommands {
    /// Generate a React + Vite + Tailwind + shadcn project for a single schema.
    Generate(SiteGenerateArgs),
}

/// Arguments for `schema-forge site generate`.
#[derive(Args)]
pub struct SiteGenerateArgs {
    /// Schema directory to scan for schema definitions.
    #[arg(short = 's', long, default_value = "schemas")]
    pub schema_dir: PathBuf,

    /// Output directory for the generated React project.
    #[arg(short = 'o', long, default_value = "site")]
    pub out_dir: PathBuf,

    /// Pick a single schema by name. Defaults to the first schema in the directory.
    #[arg(long)]
    pub schema: Option<String>,

    /// Overwrite `Preserve`-mode user files (e.g. the per-entity
    /// `src/app/pages/<entity>/{list,detail,edit}.tsx` shells). In the
    /// common case you should not need this — schema-driven data lives
    /// in the `.generated.tsx` siblings, which are always regenerated.
    #[arg(long = "force-user-files", alias = "force")]
    pub force_user_files: bool,

    /// Write into a non-empty, non-schemaforge-managed directory.
    #[arg(long)]
    pub force_init: bool,

    /// Dry-run: render in memory and report drift against what is on disk.
    /// Exits non-zero if anything would change. Intended for CI determinism checks.
    #[arg(long)]
    pub check: bool,

    /// Directory of override `.jinja` templates. Files present here shadow
    /// the ones baked into the binary, so you can iterate on generator
    /// output without recompiling the CLI. If omitted, `./site-templates`
    /// is auto-detected relative to the current working directory.
    #[arg(long = "templates-dir")]
    pub templates_dir: Option<PathBuf>,

    /// Email of the §508 program manager / accessibility coordinator for
    /// this deployment. Surfaces in the generated `/accessibility` page
    /// as the named contact and powers the mailto feedback mechanism
    /// required by OMB M-24-08 and 36 CFR 1194 §§603.2–603.3. When
    /// omitted, the page renders a visible "not configured" notice so
    /// the gap stays auditable instead of silently passing.
    #[arg(long = "accessibility-contact")]
    pub accessibility_contact: Option<String>,
}

/// Hook scaffolding subcommands.
#[derive(Subcommand)]
pub enum HooksCommands {
    /// Generate a hook service project for one or all schemas
    Generate(HooksGenerateArgs),
    /// List all `@hook(...)` annotations across the schema directory
    List(HooksListArgs),
    /// Diff hook annotations between two schema versions / directories
    Diff(HooksDiffArgs),
}

/// Arguments for `schema-forge hooks generate`.
#[derive(Args)]
pub struct HooksGenerateArgs {
    /// Schema directory to scan for `@hook(...)` annotations
    #[arg(long, default_value = "schemas")]
    pub schema_dir: PathBuf,

    /// Output directory for the generated hook service project
    #[arg(short = 'o', long, default_value = "hooks-service")]
    pub out_dir: PathBuf,

    /// Generate stubs for every annotated schema (one combined project)
    #[arg(long, conflicts_with = "schema")]
    pub all: bool,

    /// Generate stubs for a single named schema
    #[arg(long, conflicts_with = "all")]
    pub schema: Option<String>,

    /// Overwrite `Preserve`-mode user files (e.g. `src/hooks/<schema>.rs`).
    /// `Owned` files are always rewritten and do not require this flag.
    ///
    /// Accepts the legacy `--force` alias for backwards compatibility.
    #[arg(long = "force-user-files", alias = "force")]
    pub force_user_files: bool,

    /// Full regeneration mode. By default, `hooks generate` is additive: it
    /// inserts net-new schemas into `src/main.rs` and `src/hooks/mod.rs`
    /// between stable insertion markers and leaves every other byte of
    /// those files alone. `--regenerate` opts out of the additive path and
    /// rewrites every Preserve file verbatim, clobbering user edits the
    /// way `--force-user-files` does for one-off rescaffolds.
    #[arg(long)]
    pub regenerate: bool,

    /// Write into a non-empty, non-schemaforge-managed directory. Normally
    /// the generator refuses to touch such a directory to avoid clobbering
    /// unrelated files.
    #[arg(long)]
    pub force_init: bool,

    /// Dry-run mode: render the project in memory and report drift against
    /// what is on disk. Exits non-zero if anything would change. Intended
    /// for CI verification of generator determinism.
    #[arg(long)]
    pub check: bool,
}

/// Arguments for `schema-forge hooks list`.
#[derive(Args)]
pub struct HooksListArgs {
    /// Schema directory to scan
    #[arg(long, default_value = "schemas")]
    pub schema_dir: PathBuf,
}

/// Arguments for `schema-forge hooks diff`.
#[derive(Args)]
pub struct HooksDiffArgs {
    /// Old schema directory
    pub old: PathBuf,
    /// New schema directory
    pub new: PathBuf,
}

/// Token management subcommands.
#[derive(Subcommand)]
pub enum TokenCommands {
    /// Generate a 32-byte PASETO V4 symmetric key file
    InitKey(InitKeyArgs),
    /// Generate a PASETO token with the specified claims
    Generate(GenerateTokenArgs),
}

/// Arguments for `schema-forge token init-key`.
#[derive(Args)]
pub struct InitKeyArgs {
    /// Output path for the key file
    #[arg(long, default_value = "./keys/paseto.key")]
    pub output: std::path::PathBuf,
}

/// Arguments for `schema-forge token generate`.
#[derive(Args)]
pub struct GenerateTokenArgs {
    /// Path to the PASETO symmetric key file
    #[arg(long, default_value = "./keys/paseto.key")]
    pub key: std::path::PathBuf,

    /// Subject (user ID). Use "user:<id>" format.
    #[arg(long)]
    pub sub: String,

    /// Roles to include (comma-separated or repeated)
    #[arg(long, value_delimiter = ',')]
    pub roles: Vec<String>,

    /// Token lifetime in seconds (default: 3600 = 1 hour)
    #[arg(long, default_value = "3600")]
    pub lifetime: i64,

    /// Issuer claim
    #[arg(long, default_value = "schemaforge")]
    pub issuer: String,

    /// Tenant chain as JSON (e.g. '[{"schema":"Organization","entity_id":"org-1"}]')
    #[arg(long)]
    pub tenant_chain: Option<String>,

    /// String-typed custom claim: `--custom-claim-string key=value`. Repeatable.
    ///
    /// The companion to `[schema_forge.authz.principal_claims]` `type = "string"`.
    /// Values are emitted into the PASETO `custom` map verbatim — no escape
    /// sequences, no JSON parsing, no auto-coercion.
    #[arg(long = "custom-claim-string", value_parser = parse_kv)]
    pub custom_claim_string: Vec<(String, String)>,

    /// Long (i64) custom claim: `--custom-claim-long key=N`. Repeatable.
    #[arg(long = "custom-claim-long", value_parser = parse_kv_long)]
    pub custom_claim_long: Vec<(String, i64)>,

    /// Boolean custom claim: `--custom-claim-bool key=true|false`. Repeatable.
    #[arg(long = "custom-claim-bool", value_parser = parse_kv_bool)]
    pub custom_claim_bool: Vec<(String, bool)>,

    /// Set-of-string custom claim: `--custom-claim-set-string key=a,b,c`. Repeatable.
    ///
    /// Comma-separated values are split into a JSON array of strings. Empty
    /// values (`key=`) emit an empty array.
    #[arg(long = "custom-claim-set-string", value_parser = parse_kv_set_string)]
    pub custom_claim_set_string: Vec<(String, Vec<String>)>,
}

/// Parse `key=value` into `(String, String)`. Splits on the first `=` only
/// so values may contain `=` characters.
fn parse_kv(raw: &str) -> Result<(String, String), String> {
    let (k, v) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected key=value, got '{raw}'"))?;
    if k.is_empty() {
        return Err(format!("empty key in '{raw}'"));
    }
    Ok((k.to_string(), v.to_string()))
}

fn parse_kv_long(raw: &str) -> Result<(String, i64), String> {
    let (k, v) = parse_kv(raw)?;
    let n: i64 = v
        .parse()
        .map_err(|e| format!("invalid integer for '{k}': {e}"))?;
    Ok((k, n))
}

fn parse_kv_bool(raw: &str) -> Result<(String, bool), String> {
    let (k, v) = parse_kv(raw)?;
    let b = match v.as_str() {
        "true" => true,
        "false" => false,
        other => return Err(format!("invalid bool for '{k}': '{other}' (expected true/false)")),
    };
    Ok((k, b))
}

fn parse_kv_set_string(raw: &str) -> Result<(String, Vec<String>), String> {
    let (k, v) = parse_kv(raw)?;
    let items: Vec<String> = if v.is_empty() {
        Vec::new()
    } else {
        v.split(',').map(|s| s.to_string()).collect()
    };
    Ok((k, items))
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

    /// Admin username used to bootstrap the initial user on first run.
    #[arg(long = "admin-user", env = "FORGE_ADMIN_USER", default_value = "admin")]
    pub admin_user: String,

    /// Admin password used to bootstrap the initial user on first run.
    #[arg(long = "admin-password", env = "FORGE_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,

    /// Seed the bundled SchemaForge demo personas (alice/bob/charlie/dana/eve)
    /// after admin bootstrap.
    ///
    /// SECURITY: each demo account is created with the literal password
    /// `"password"`. Enable ONLY for the local `task demo` flow. Production
    /// and downstream AMI deployments must leave this disabled — which is
    /// the default. See https://github.com/govcraft/schemaforge/issues/53.
    #[arg(
        long = "seed-demo-users",
        env = "FORGE_SEED_DEMO_USERS",
        default_value_t = false
    )]
    pub seed_demo_users: bool,

    /// Enable permissive CORS for local development.
    ///
    /// Allows all origins via acton-service's `with_development_cors()`.
    /// DO NOT use in production.
    #[arg(long = "dev-cors")]
    pub dev_cors: bool,

    /// Do not serve the bundled ops console, even in a build that embeds it.
    ///
    /// The console (when compiled in via the `embedded-console` feature) is
    /// served same-origin at `/` as the router fallback. Pass this to run the
    /// JSON API only. No effect on builds without the console embedded.
    #[arg(long = "no-console")]
    pub no_console: bool,

    /// Path to a `role_ranks.toml` defining the role-name → numeric-rank
    /// hierarchy. The runtime no-upward-visibility guard for user mgmt
    /// (`principal.role_rank >= resource.role_rank`) reads from this file.
    /// A missing file is treated as the empty hierarchy
    /// (only `platform_admin` carries a rank).
    #[arg(long = "role-ranks", default_value = "policies/role_ranks.toml")]
    pub role_ranks: PathBuf,

    /// Directory containing additional `*.cedar` policies to merge into
    /// the runtime bundle (e.g. `policies/custom/`). Overrides
    /// `[schema_forge.authz] custom_policies_dir` in config.toml. When
    /// neither is set, the daemon auto-discovers `policies/custom`
    /// relative to the working directory if that directory exists.
    /// Missing explicitly-named directories are treated as empty.
    #[arg(long = "custom-dir")]
    pub custom_dir: Option<PathBuf>,
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

    /// Validate the Cedar bundle (generated + custom) without applying it
    Validate(PolicyValidateArgs),
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

/// Arguments for `schema-forge policies validate`.
///
/// Compiles the Cedar schema + generated policies + every `*.cedar` file
/// under `--custom-dir` into a `PolicyStore` and runs strict-mode
/// validation. Exits non-zero on any error so CI / pre-deploy hooks can
/// gate releases on a passing bundle.
#[derive(Args)]
pub struct PolicyValidateArgs {
    /// Directories to scan for SchemaForge `.sf` schema files.
    #[arg(default_value = "schemas/")]
    pub schema_paths: Vec<PathBuf>,

    /// Directory containing additional `*.cedar` policies to merge into
    /// the bundle (e.g. `policies/custom/`). Missing directories are
    /// treated as empty.
    #[arg(long = "custom-dir")]
    pub custom_dir: Option<PathBuf>,

    /// Path to a `role_ranks.toml` defining the role hierarchy. Missing
    /// files are treated as the empty (platform_admin-only) hierarchy.
    #[arg(long = "role-ranks", default_value = "policies/role_ranks.toml")]
    pub role_ranks: PathBuf,
}

/// Arguments for `schema-forge completions`.
#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_parser = ["bash", "zsh", "fish", "powershell", "elvish"])]
    pub shell: String,
}

// ---------------------------------------------------------------------------
// Entity REST command group (HTTP client against a running instance)
// ---------------------------------------------------------------------------

/// Connection options shared by every `entity` verb and by `login`.
///
/// Deliberately separate from the global `--db-url` family: this group talks
/// to a *running server over HTTP*, never to a database. The Bearer token is
/// never accepted as a flag — only via `--token-stdin`, `--token-file`, the
/// `SCHEMAFORGE_TOKEN` environment variable, or the cached `login` token —
/// so it stays out of `ps` output and shell history. (`login` ignores the
/// token-source fields; it is acquiring a token.)
#[derive(Args, Debug, Clone)]
pub struct EntityConnectionArgs {
    /// Base URL of the running instance (e.g. https://forge.agency.gov). The
    /// versioned API path is appended automatically. [env: SCHEMAFORGE_SERVER]
    #[arg(long, env = "SCHEMAFORGE_SERVER")]
    pub server: Option<String>,

    /// API version path segment.
    #[arg(long = "api-version", default_value = "v1")]
    pub api_version: String,

    /// Read the Bearer token from this file (preferred on shared machines).
    #[arg(long = "token-file")]
    pub token_file: Option<PathBuf>,

    /// Read the Bearer token from stdin (highest precedence).
    #[arg(long = "token-stdin")]
    pub token_stdin: bool,

    /// PEM CA certificate for verifying a private-PKI server certificate.
    #[arg(long = "ca-cert")]
    pub ca_cert: Option<PathBuf>,

    /// Skip TLS certificate verification. INSECURE — warns on every call and
    /// must never be used against production.
    #[arg(long = "insecure")]
    pub insecure: bool,

    /// Per-request timeout in seconds.
    #[arg(long = "timeout")]
    pub timeout: Option<u64>,
}

/// Field input shared by `create`, `replace`, and `patch`.
#[derive(Args, Debug, Clone)]
pub struct EntityInputArgs {
    /// Set a field. `--set name=Alice` types the value (numbers and
    /// true/false are detected; everything else stays a string). Use
    /// `field:=json` for explicit JSON — arrays, objects, relation lists:
    /// `--set 'tags:=["a","b"]'`. Repeatable.
    #[arg(long = "set", value_name = "FIELD=VALUE")]
    pub set: Vec<String>,

    /// Full request body as JSON: a `{...}` literal, `@file.json`, or `-`
    /// for stdin. A bare field map is wrapped in `{ "fields": ... }`
    /// automatically. Mutually combinable with `--set` (set overlays data).
    #[arg(long = "data", value_name = "JSON|@FILE|-")]
    pub data: Option<String>,
}

/// `entity` subcommands. Verbs map to HTTP semantics: `replace` is PUT (the
/// complete entity, required fields enforced) and `patch` is PATCH (a partial
/// merge).
#[derive(Subcommand)]
pub enum EntityCommands {
    /// List entities, optionally filtered, sorted, and paginated.
    ///
    /// The args are boxed: `EntityListArgs` carries the connection options
    /// plus nine filter vectors, so inlining it would bloat every variant of
    /// this enum (clippy::large_enum_variant).
    List(Box<EntityListArgs>),
    /// Fetch a single entity by ID.
    Get(Box<EntityGetArgs>),
    /// Create a new entity.
    Create(Box<EntityCreateArgs>),
    /// Replace an entity (PUT — full entity, all required fields present).
    Replace(Box<EntityWriteArgs>),
    /// Partially update an entity (PATCH — only the supplied fields change).
    Patch(Box<EntityWriteArgs>),
    /// Delete an entity by ID.
    Delete(Box<EntityDeleteArgs>),
    /// Run an advanced JSON-filter query (POST .../entities/query).
    Query(Box<EntityQueryArgs>),
    /// Upload to or download from a `file` field (S3-backed attachment).
    File {
        #[command(subcommand)]
        command: EntityFileCommands,
    },
}

/// `entity file` subcommands.
///
/// A `file` field is an S3-backed attachment. The runtime never proxies upload
/// bytes: `upload` performs the presigned handshake (mint → PUT direct to S3 →
/// confirm), and `download` follows the runtime's presigned redirect (or
/// streamed proxy) to the bytes. Clearing or replacing an attachment is not yet
/// supported — there is no server route for it.
#[derive(Subcommand)]
pub enum EntityFileCommands {
    /// Upload a local file to a `file` field via the presigned handshake.
    ///
    /// Boxed for the same reason as the CRUD verbs: the args carry the full
    /// connection block plus path/mime/filename, so inlining would bloat every
    /// enum variant (clippy::large_enum_variant).
    Upload(Box<EntityFileUploadArgs>),
    /// Download a `file` field's bytes to a path or stdout.
    Download(Box<EntityFileDownloadArgs>),
}

/// Arguments for `entity file upload`.
#[derive(Args)]
pub struct EntityFileUploadArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name (e.g. Document).
    pub schema: String,
    /// Entity ID.
    pub id: String,
    /// File field name on the schema.
    pub field: String,
    /// Path to the local file to upload.
    pub path: PathBuf,
    /// MIME type to assert. Overrides extension-based detection. The server
    /// validates this against the field's allowlist, so it must be accurate.
    #[arg(long)]
    pub mime: Option<String>,
    /// Filename recorded in the object key. Defaults to the path's file name.
    #[arg(long)]
    pub filename: Option<String>,
}

/// Arguments for `entity file download`.
#[derive(Args)]
pub struct EntityFileDownloadArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name.
    pub schema: String,
    /// Entity ID.
    pub id: String,
    /// File field name on the schema.
    pub field: String,
    /// Output path. `-` streams to stdout. Defaults to a sanitized basename
    /// derived from the server's response, falling back to the field name.
    #[arg(long)]
    pub out: Option<String>,
}

/// Arguments for `entity list`.
#[derive(Args)]
pub struct EntityListArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,

    /// Schema name (e.g. Contact).
    pub schema: String,

    /// Raw filter operands in the server's `field__op=value` grammar, passed
    /// through verbatim (e.g. `age__gt=25` `status__in=a,b`). A bare
    /// `field=value` means equality.
    #[arg(value_name = "FIELD__OP=VALUE")]
    pub filters: Vec<String>,

    /// Equality filter convenience: `--eq field=value`. Repeatable.
    #[arg(long = "eq", value_name = "F=V")]
    pub eq: Vec<String>,
    /// Not-equal filter: `--ne field=value`. Repeatable.
    #[arg(long = "ne", value_name = "F=V")]
    pub ne: Vec<String>,
    /// Greater-than filter: `--gt field=value`. Repeatable.
    #[arg(long = "gt", value_name = "F=V")]
    pub gt: Vec<String>,
    /// Greater-or-equal filter: `--gte field=value`. Repeatable.
    #[arg(long = "gte", value_name = "F=V")]
    pub gte: Vec<String>,
    /// Less-than filter: `--lt field=value`. Repeatable.
    #[arg(long = "lt", value_name = "F=V")]
    pub lt: Vec<String>,
    /// Less-or-equal filter: `--lte field=value`. Repeatable.
    #[arg(long = "lte", value_name = "F=V")]
    pub lte: Vec<String>,
    /// Substring filter: `--contains field=value`. Repeatable.
    #[arg(long = "contains", value_name = "F=V")]
    pub contains: Vec<String>,
    /// Prefix filter: `--startswith field=value`. Repeatable.
    #[arg(long = "startswith", value_name = "F=V")]
    pub startswith: Vec<String>,
    /// Set-membership filter: `--in field=a,b,c`. Repeatable.
    #[arg(long = "in", value_name = "F=A,B,C")]
    pub in_: Vec<String>,

    /// Sort spec: `-age,name` (prefix `-` = descending) or `age:desc`.
    /// `allow_hyphen_values` lets the leading `-` be read as part of the
    /// value rather than a flag.
    #[arg(long, allow_hyphen_values = true)]
    pub sort: Option<String>,
    /// Field projection (comma-separated).
    #[arg(long)]
    pub fields: Option<String>,
    /// Maximum number of entities to return.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Number of entities to skip.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Skip the total-count computation (sends count=false).
    #[arg(long = "no-count")]
    pub no_count: bool,
    /// Skip relation display resolution (sends resolve=false).
    #[arg(long = "no-resolve")]
    pub no_resolve: bool,
}

/// Arguments for `entity get`.
#[derive(Args)]
pub struct EntityGetArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name.
    pub schema: String,
    /// Entity ID.
    pub id: String,
    /// Field projection (comma-separated).
    #[arg(long)]
    pub fields: Option<String>,
    /// Skip relation display resolution (sends resolve=false).
    #[arg(long = "no-resolve")]
    pub no_resolve: bool,
}

/// Arguments for `entity create`.
#[derive(Args)]
pub struct EntityCreateArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name.
    pub schema: String,
    #[command(flatten)]
    pub input: EntityInputArgs,
    /// Print the resolved request (method, URL, body) without sending it.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// Arguments for `entity replace` (PUT) and `entity patch` (PATCH).
#[derive(Args)]
pub struct EntityWriteArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name.
    pub schema: String,
    /// Entity ID.
    pub id: String,
    #[command(flatten)]
    pub input: EntityInputArgs,
    /// Print the resolved request (method, URL, body) without sending it.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// Arguments for `entity delete`.
#[derive(Args)]
pub struct EntityDeleteArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name.
    pub schema: String,
    /// Entity ID.
    pub id: String,
    /// Skip the confirmation prompt. Required in non-interactive contexts.
    #[arg(long = "yes", short = 'y')]
    pub yes: bool,
    /// Print the resolved request without sending it.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// Arguments for `entity query`.
#[derive(Args)]
pub struct EntityQueryArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Schema name.
    pub schema: String,
    /// JSON filter body: a `{...}` literal, `@file.json`, or `-` for stdin.
    #[arg(long = "filter", value_name = "JSON|@FILE|-")]
    pub filter: Option<String>,
    /// Sort spec: `-age,name` or `age:desc`.
    #[arg(long, allow_hyphen_values = true)]
    pub sort: Option<String>,
    /// Field projection (comma-separated).
    #[arg(long)]
    pub fields: Option<String>,
    /// Maximum number of entities to return.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Number of entities to skip.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Skip the total-count computation (sends count=false).
    #[arg(long = "no-count")]
    pub no_count: bool,
    /// Skip relation display resolution (sends resolve=false).
    #[arg(long = "no-resolve")]
    pub no_resolve: bool,
}

/// Arguments for `schemaforge login`.
#[derive(Args)]
pub struct LoginArgs {
    #[command(flatten)]
    pub conn: EntityConnectionArgs,
    /// Username to authenticate.
    #[arg(long, short = 'u')]
    pub username: String,
    /// Read the password from stdin instead of prompting interactively.
    #[arg(long = "password-stdin")]
    pub password_stdin: bool,
    /// Do not cache the token to the XDG state directory; print only.
    #[arg(long = "no-save")]
    pub no_save: bool,
    /// Also write the raw token to stdout (pipeable).
    #[arg(long = "print-token")]
    pub print_token: bool,
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
        let cli = Cli::try_parse_from(["schemaforge", "completions", "bash"]).unwrap();
        assert!(matches!(cli.command, Commands::Completions(_)));
    }

    #[test]
    fn parse_global_verbose() {
        let cli = Cli::try_parse_from(["schemaforge", "-vvv", "completions", "bash"]).unwrap();
        assert_eq!(cli.global.verbose, 3);
    }

    #[test]
    fn parse_global_quiet() {
        let cli = Cli::try_parse_from(["schemaforge", "-q", "completions", "bash"]).unwrap();
        assert!(cli.global.quiet);
    }

    #[test]
    fn parse_global_format_json() {
        let cli = Cli::try_parse_from(["schemaforge", "--format", "json", "completions", "bash"])
            .unwrap();
        assert_eq!(cli.global.format, "json");
    }

    #[test]
    fn parse_init_command() {
        let cli =
            Cli::try_parse_from(["schemaforge", "init", "my-project", "-t", "minimal"]).unwrap();
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
        let cli = Cli::try_parse_from(["schemaforge", "parse", "--print", "schemas/"]).unwrap();
        if let Commands::Parse(args) = cli.command {
            assert!(args.print_ast);
            assert_eq!(args.paths, vec![PathBuf::from("schemas/")]);
        } else {
            panic!("expected Parse command");
        }
    }

    #[test]
    fn parse_apply_command_dry_run() {
        let cli = Cli::try_parse_from(["schemaforge", "apply", "--dry-run"]).unwrap();
        if let Commands::Apply(args) = cli.command {
            assert!(args.dry_run);
            assert!(!args.force);
        } else {
            panic!("expected Apply command");
        }
    }

    #[test]
    fn parse_migrate_command() {
        let cli =
            Cli::try_parse_from(["schemaforge", "migrate", "--execute", "--schema", "Contact"])
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
        let cli = Cli::try_parse_from(["schemaforge", "inspect", "Contact", "--detail"]).unwrap();
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
            Cli::try_parse_from(["schemaforge", "export", "openapi", "-o", "api.json"]).unwrap();
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
        let cli = Cli::try_parse_from(["schemaforge", "policies", "list", "Contact"]).unwrap();
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
            "schemaforge",
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
    fn parse_policies_validate() {
        let cli = Cli::try_parse_from([
            "schemaforge",
            "policies",
            "validate",
            "schemas/",
            "--custom-dir",
            "policies/custom",
            "--role-ranks",
            "policies/role_ranks.toml",
        ])
        .unwrap();
        if let Commands::Policies {
            command: PolicyCommands::Validate(args),
        } = cli.command
        {
            assert_eq!(args.schema_paths, vec![PathBuf::from("schemas/")]);
            assert_eq!(args.custom_dir, Some(PathBuf::from("policies/custom")));
            assert_eq!(args.role_ranks, PathBuf::from("policies/role_ranks.toml"));
        } else {
            panic!("expected Policies Validate command");
        }
    }

    #[test]
    fn parse_bootstrap_admin_with_flags() {
        let cli = Cli::try_parse_from([
            "schemaforge",
            "bootstrap-admin",
            "--username",
            "ops",
            "--password",
            "s3cret",
            "--display-name",
            "Ops Team",
        ])
        .unwrap();
        if let Commands::BootstrapAdmin(args) = cli.command {
            assert_eq!(args.username, "ops");
            assert_eq!(args.password, "s3cret");
            assert_eq!(args.display_name, "Ops Team");
        } else {
            panic!("expected BootstrapAdmin command");
        }
    }

    #[test]
    fn parse_bootstrap_admin_password_is_required() {
        // Without --password and no env var, clap must reject the invocation
        // so provisioning scripts fail loud rather than seeding an empty
        // password.
        std::env::remove_var("SCHEMA_FORGE_BOOTSTRAP_ADMIN_PASSWORD");
        let result = Cli::try_parse_from(["schemaforge", "bootstrap-admin"]);
        assert!(result.is_err(), "missing --password should be a parse error");
    }

    #[test]
    fn parse_serve_command() {
        let cli = Cli::try_parse_from([
            "schemaforge",
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
    fn verbose_and_quiet_conflict() {
        let result = Cli::try_parse_from(["schemaforge", "-v", "-q", "completions", "bash"]);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_format_rejected() {
        let result = Cli::try_parse_from(["schemaforge", "--format", "xml", "completions", "bash"]);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_shell_rejected() {
        let result = Cli::try_parse_from(["schemaforge", "completions", "tcsh"]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_entity_list_with_filters_and_conn() {
        let cli = Cli::try_parse_from([
            "schemaforge",
            "entity",
            "list",
            "Contact",
            "age__gt=25",
            "--eq",
            "name=Alice",
            "--server",
            "https://forge.example.gov",
            "--sort",
            "-age,name",
            "--limit",
            "10",
            "--no-resolve",
        ])
        .unwrap();
        let Commands::Entity {
            command: EntityCommands::List(args),
        } = cli.command
        else {
            panic!("expected Entity List command");
        };
        assert_eq!(args.schema, "Contact");
        assert_eq!(args.filters, vec!["age__gt=25".to_string()]);
        assert_eq!(args.eq, vec!["name=Alice".to_string()]);
        assert_eq!(args.conn.server.as_deref(), Some("https://forge.example.gov"));
        assert_eq!(args.sort.as_deref(), Some("-age,name"));
        assert_eq!(args.limit, Some(10));
        assert!(args.no_resolve);
    }

    #[test]
    fn parse_entity_create_with_set() {
        let cli = Cli::try_parse_from([
            "schemaforge",
            "entity",
            "create",
            "User",
            "--set",
            "name=Alice",
            "--set",
            "roles:=[\"staff\"]",
        ])
        .unwrap();
        let Commands::Entity {
            command: EntityCommands::Create(args),
        } = cli.command
        else {
            panic!("expected Entity Create command");
        };
        assert_eq!(args.schema, "User");
        assert_eq!(
            args.input.set,
            vec!["name=Alice".to_string(), "roles:=[\"staff\"]".to_string()]
        );
    }

    #[test]
    fn parse_entity_delete_requires_id_and_takes_yes() {
        let cli = Cli::try_parse_from([
            "schemaforge",
            "entity",
            "delete",
            "User",
            "user_01abc",
            "--yes",
        ])
        .unwrap();
        let Commands::Entity {
            command: EntityCommands::Delete(args),
        } = cli.command
        else {
            panic!("expected Entity Delete command");
        };
        assert_eq!(args.schema, "User");
        assert_eq!(args.id, "user_01abc");
        assert!(args.yes);
    }

    #[test]
    fn entity_has_no_token_flag() {
        // The Bearer token must never be acceptable as a flag (it would leak
        // via `ps` and shell history). Only --token-file / --token-stdin / env.
        let result = Cli::try_parse_from([
            "schemaforge",
            "entity",
            "get",
            "User",
            "user_01abc",
            "--token",
            "v4.local.secret",
        ]);
        assert!(result.is_err(), "--token must not be a recognized flag");
    }

    #[test]
    fn parse_login_command() {
        let cli = Cli::try_parse_from([
            "schemaforge",
            "login",
            "--server",
            "https://forge.example.gov",
            "--username",
            "admin",
            "--print-token",
        ])
        .unwrap();
        let Commands::Login(args) = cli.command else {
            panic!("expected Login command");
        };
        assert_eq!(args.username, "admin");
        assert!(args.print_token);
        assert_eq!(args.conn.server.as_deref(), Some("https://forge.example.gov"));
    }
}
