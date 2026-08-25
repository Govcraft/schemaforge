//! Database connection parameter resolution from acton-service's canonical
//! `Config<SchemaForgeConfig>`.
//!
//! Schema-forge does not maintain a parallel config layer. The single source
//! of truth for runtime configuration is acton-service's `Config<T>`, loaded
//! from `config.toml` (XDG-discovered or explicitly via `--config <path>`)
//! and overlaid with `ACTON_*` environment variables. CLI flags layer on top
//! by mutating the same `Config<T>` via [`apply_cli_overrides`].
//!
//! Issue #47: an earlier design loaded `[database]` independently in
//! schema-forge AND in acton-service. CLI `--db-url` only patched
//! schema-forge's copy; acton-service kept its config-file URL — two pools
//! to two databases, silently. Sharing one `Config` removes the bug class
//! by construction: there is exactly one URL, so the schema-forge backend
//! pool and acton-service's pool can never disagree.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use acton_service::config::Config;
use schema_forge_acton::config::ClientConfig;
use schema_forge_acton::SchemaForgeConfig;
use schema_forge_signing::{SigningConfig, SigningMode, VerifyPolicy};

use crate::cli::{EntityConnectionArgs, GlobalOpts};
use crate::error::CliError;

/// Default SurrealDB URL when no config and no CLI flag are supplied.
///
/// Matches the behavior of the pre-#47 [`load_config`] fallback so that
/// `schemaforge serve` keeps working out-of-the-box for development setups
/// that never wrote a `config.toml`.
const DEFAULT_DEV_SURREALDB_URL: &str = "ws://localhost:8000";
const DEFAULT_SURREALDB_NAMESPACE: &str = "schemaforge";
const DEFAULT_SURREALDB_DATABASE: &str = "dev";

/// SurrealDB-specific connection parameters resolved from svc_config + CLI flags.
#[derive(Debug, Clone)]
pub struct SurrealDbParams {
    pub url: String,
    pub namespace: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// PostgreSQL-specific connection parameters resolved from svc_config + CLI flags.
#[derive(Debug, Clone)]
pub struct PostgresParams {
    pub url: String,
}

/// Microsoft SQL Server parameters from acton-service's canonical database section.
#[derive(Debug, Clone)]
pub struct MssqlParams {
    pub config: acton_service::config::DatabaseConfig,
}

/// Resolved backend connection parameters. PostgreSQL is selected by URL
/// scheme and SQL Server by its ADO-style connection string.
#[derive(Debug, Clone)]
pub enum DbParams {
    Surrealdb(SurrealDbParams),
    Postgres(PostgresParams),
    Mssql(MssqlParams),
}

impl DbParams {
    /// The connection URL, regardless of backend.
    pub fn url(&self) -> &str {
        match self {
            DbParams::Surrealdb(p) => &p.url,
            DbParams::Postgres(p) => &p.url,
            DbParams::Mssql(p) => &p.config.url,
        }
    }
}

impl std::fmt::Display for DbParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbParams::Surrealdb(p) => {
                let user = p.username.as_deref().unwrap_or("(anonymous)");
                let masked_pass = if p.password.is_some() {
                    "***"
                } else {
                    "(none)"
                };
                write!(
                    f,
                    "surrealdb {}/{}@{} (user={user}, pass={masked_pass})",
                    p.namespace, p.database, p.url
                )
            }
            DbParams::Postgres(p) => write!(f, "postgres {}", p.url),
            DbParams::Mssql(p) => write!(f, "mssql {}", p.config.url),
        }
    }
}

/// Load the canonical `Config<SchemaForgeConfig>` and apply CLI overrides.
///
/// Resolution order, highest priority first:
/// 1. CLI flags (`--db-url`, `--db-ns`, `--db-name`)
/// 2. `SCHEMA_FORGE_DB_*` env vars (surfaced as flag values by clap)
/// 3. `ACTON_*` env vars (acton-service's overlay)
/// 4. `config.toml` from `--config <path>` or XDG discovery
/// 5. Built-in defaults
///
/// The returned `Config` is fully resolved — both schema-forge's backend
/// connection (via [`resolve_db_params`]) and acton-service's pool (via
/// `ServiceBuilder::with_config`) read from the same struct.
pub fn load_svc_config(global: &GlobalOpts) -> Result<Config<SchemaForgeConfig>, CliError> {
    let mut svc = match global.config.as_deref() {
        Some(path) => load_svc_config_from_path(path)?,
        None => Config::<SchemaForgeConfig>::load_for_service("schemaforge").map_err(|e| {
            CliError::Config {
                message: format!("failed to load configuration: {e}"),
            }
        })?,
    };
    apply_cli_overrides(&mut svc, global)?;
    Ok(svc)
}

fn load_svc_config_from_path(path: &Path) -> Result<Config<SchemaForgeConfig>, CliError> {
    let path_str = path.to_str().ok_or_else(|| CliError::Config {
        message: format!("config path is not valid UTF-8: {}", path.display()),
    })?;
    Config::<SchemaForgeConfig>::load_from(path_str).map_err(|e| CliError::Config {
        message: format!("failed to load {}: {e}", path.display()),
    })
}

/// Mutate `svc` so its database section reflects CLI flag overrides.
///
/// The override is total: when `--db-url` is provided, the matching backend
/// section is set to the CLI URL and the *other* backend section is cleared.
/// This prevents the silent dual-pool spawn that motivated #47 — without
/// the clear, an operator who switches from SurrealDB to Postgres on the
/// command line would still leave acton-service spawning a SurrealDB pool
/// from leftover config.
fn apply_cli_overrides(
    svc: &mut Config<SchemaForgeConfig>,
    global: &GlobalOpts,
) -> Result<(), CliError> {
    if let Some(cli_url) = global.db_url.as_deref() {
        if is_postgres_url(cli_url) || is_mssql_connection_string(cli_url) {
            apply_cli_database_url(svc, cli_url);
        } else {
            apply_cli_surrealdb_url(svc, cli_url);
        }
    }

    apply_cli_surrealdb_naming(svc, global)?;
    Ok(())
}

fn apply_cli_database_url(svc: &mut Config<SchemaForgeConfig>, url: &str) {
    match svc.database.as_mut() {
        Some(db) => db.url = url.to_string(),
        None => {
            // Construct via serde defaults so we pick up the same pool-sizing
            // values acton-service would have applied if `[database]` had
            // been present in config.toml.
            let toml_src = format!("url = {}\n", toml::Value::String(url.to_string()));
            let cfg: acton_service::config::DatabaseConfig = toml::from_str(&toml_src)
                .expect("hard-coded TOML with serde defaults must deserialize");
            svc.database = Some(cfg);
        }
    }
    #[cfg(feature = "surrealdb")]
    {
        svc.surrealdb = None;
    }
}

#[cfg(feature = "surrealdb")]
fn apply_cli_surrealdb_url(svc: &mut Config<SchemaForgeConfig>, url: &str) {
    match svc.surrealdb.as_mut() {
        Some(s) => s.url = url.to_string(),
        None => {
            let toml_src = format!("url = {}\n", toml::Value::String(url.to_string()));
            let cfg: acton_service::config::SurrealDbConfig = toml::from_str(&toml_src)
                .expect("hard-coded TOML with serde defaults must deserialize");
            svc.surrealdb = Some(cfg);
        }
    }
    svc.database = None;
}

#[cfg(not(feature = "surrealdb"))]
fn apply_cli_surrealdb_url(_svc: &mut Config<SchemaForgeConfig>, url: &str) {
    // Reaching this branch means the user passed a non-postgres `--db-url`
    // to a binary built without SurrealDB support. Leave svc unchanged;
    // resolve_db_params will surface an error with the offending URL when
    // it inspects what's actually configured.
    let _ = url;
}

#[cfg(feature = "surrealdb")]
fn apply_cli_surrealdb_naming(
    svc: &mut Config<SchemaForgeConfig>,
    global: &GlobalOpts,
) -> Result<(), CliError> {
    if global.db_ns.is_none() && global.db_name.is_none() {
        return Ok(());
    }
    let Some(s) = svc.surrealdb.as_mut() else {
        return Err(CliError::Config {
            message: "--db-ns / --db-name require a SurrealDB backend; \
                      pass --db-url ws://... or set [surrealdb] in config.toml"
                .to_string(),
        });
    };
    if let Some(ns) = &global.db_ns {
        s.namespace = ns.clone();
    }
    if let Some(db) = &global.db_name {
        s.database = db.clone();
    }
    Ok(())
}

#[cfg(not(feature = "surrealdb"))]
fn apply_cli_surrealdb_naming(
    _svc: &mut Config<SchemaForgeConfig>,
    global: &GlobalOpts,
) -> Result<(), CliError> {
    if global.db_ns.is_some() || global.db_name.is_some() {
        return Err(CliError::Config {
            message: "--db-ns / --db-name require a SurrealDB-enabled build".to_string(),
        });
    }
    Ok(())
}

/// Read the resolved backend parameters out of `svc`.
///
/// Selection rule:
/// - `[database]` with a PostgreSQL URL → PostgreSQL.
/// - `[database]` with an ADO connection string → Microsoft SQL Server.
/// - `[surrealdb]` present → SurrealDB.
/// - Both present → error (ambiguous; the operator must remove one).
/// - Neither present → SurrealDB at [`DEFAULT_DEV_SURREALDB_URL`] for
///   developer ergonomics. This matches the pre-#47 fallback.
pub fn resolve_db_params(svc: &Config<SchemaForgeConfig>) -> Result<DbParams, CliError> {
    let has_pg = svc.database.is_some();
    let has_surreal = surrealdb_section_present(svc);

    if has_pg && has_surreal {
        return Err(CliError::Config {
            message: "config has both [database] (postgres) and [surrealdb] sections; \
                      keep only one or override with --db-url"
                .to_string(),
        });
    }

    if let Some(db) = &svc.database {
        if is_mssql_connection_string(&db.url) {
            return Ok(DbParams::Mssql(MssqlParams { config: db.clone() }));
        }
        return Ok(DbParams::Postgres(PostgresParams {
            url: db.url.clone(),
        }));
    }

    Ok(surrealdb_params_or_default(svc))
}

#[cfg(feature = "surrealdb")]
fn surrealdb_section_present(svc: &Config<SchemaForgeConfig>) -> bool {
    svc.surrealdb.is_some()
}

#[cfg(not(feature = "surrealdb"))]
fn surrealdb_section_present(_svc: &Config<SchemaForgeConfig>) -> bool {
    false
}

#[cfg(feature = "surrealdb")]
fn surrealdb_params_or_default(svc: &Config<SchemaForgeConfig>) -> DbParams {
    let Some(s) = &svc.surrealdb else {
        return DbParams::Surrealdb(default_dev_surrealdb_params());
    };
    DbParams::Surrealdb(SurrealDbParams {
        url: s.url.clone(),
        namespace: s.namespace.clone(),
        database: s.database.clone(),
        username: s.username.clone(),
        password: s.password.clone(),
    })
}

#[cfg(not(feature = "surrealdb"))]
fn surrealdb_params_or_default(_svc: &Config<SchemaForgeConfig>) -> DbParams {
    DbParams::Surrealdb(default_dev_surrealdb_params())
}

fn default_dev_surrealdb_params() -> SurrealDbParams {
    SurrealDbParams {
        url: DEFAULT_DEV_SURREALDB_URL.to_string(),
        namespace: DEFAULT_SURREALDB_NAMESPACE.to_string(),
        database: DEFAULT_SURREALDB_DATABASE.to_string(),
        username: None,
        password: None,
    }
}

/// Resolve a [`VerifyPolicy`] for schema-loading commands.
///
/// Resolution order:
/// 1. If `global.no_verify` is set, return [`VerifyPolicy::off`] —
///    the operator has explicitly opted out for this invocation. The
///    CLI surfaces a one-time warning so this doesn't drift into a
///    silent default.
/// 2. If `global.trust_policy` points at an external TOML file, load
///    that file as a standalone [`SigningConfig`] (handy when the
///    deployment config lives elsewhere and only the trust policy is
///    pinned per environment).
/// 3. Otherwise, use the `[schema_forge.signing]` section from the
///    already-loaded `Config<SchemaForgeConfig>`.
///
/// Refuses to honour `--no-verify` when the resolved policy is
/// `mode = "enforce"`. Operators who want a working escape hatch in
/// strict environments must instead set `SCHEMAFORGE_ALLOW_NO_VERIFY=1`
/// — the env var is the explicit acknowledgement that they own the
/// risk for this command. This matches the design used for
/// production-grade auth bypasses.
pub fn build_verify_policy(
    svc_config: &Config<SchemaForgeConfig>,
    global: &GlobalOpts,
) -> Result<VerifyPolicy, CliError> {
    let signing_config = resolve_signing_config(svc_config, global)?;

    if global.no_verify {
        let allow_in_enforce = std::env::var("SCHEMAFORGE_ALLOW_NO_VERIFY")
            .map(|v| v == "1")
            .unwrap_or(false);
        if signing_config.mode == SigningMode::Enforce && !allow_in_enforce {
            return Err(CliError::Config {
                message: "--no-verify refused: schema_forge.signing.mode is \"enforce\". \
                     Set SCHEMAFORGE_ALLOW_NO_VERIFY=1 to override (audit the \
                     reason in your change log)."
                    .into(),
            });
        }
        return Ok(VerifyPolicy::off());
    }

    Ok(VerifyPolicy::from_config(&signing_config)?)
}

/// Where the trust-policy bytes actually come from. Pulled out from
/// [`build_verify_policy`] so the `verify` subcommand can introspect
/// the resolved policy without re-running `--no-verify` checks.
pub fn resolve_signing_config(
    svc_config: &Config<SchemaForgeConfig>,
    global: &GlobalOpts,
) -> Result<SigningConfig, CliError> {
    if let Some(path) = &global.trust_policy {
        let text = std::fs::read_to_string(path).map_err(|e| CliError::Io {
            path: path.clone(),
            source: e,
        })?;
        let cfg: SigningConfig = toml::from_str(&text).map_err(|e| CliError::Config {
            message: format!("failed to parse trust policy {}: {e}", path.display()),
        })?;
        return Ok(cfg);
    }
    Ok(svc_config.custom.schema_forge.signing.clone())
}

/// Detect PostgreSQL URLs by scheme.
pub fn is_postgres_url(url: &str) -> bool {
    url.starts_with("postgres://") || url.starts_with("postgresql://")
}

/// Whether a value uses Tiberius' ADO-style SQL Server connection syntax.
pub fn is_mssql_connection_string(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("server=") || lower.contains("data source=")
}

// ---------------------------------------------------------------------------
// Entity HTTP command connection resolution
// ---------------------------------------------------------------------------

/// Default server origin for the entity HTTP commands when neither a flag,
/// env var, nor `[schema_forge.client]` supplies one. Matches the `serve`
/// defaults (host 127.0.0.1, port 3000).
const DEFAULT_CLIENT_SERVER: &str = "http://127.0.0.1:3000";

/// Default per-request timeout for entity HTTP commands.
const DEFAULT_CLIENT_TIMEOUT_SECS: u64 = 30;

/// Fully-resolved connection settings for the entity HTTP command group.
///
/// `Debug` is implemented by hand to redact the token: it must never appear
/// in logs, panics, or `-vvv` output.
#[derive(Clone)]
pub struct ResolvedClient {
    /// Server origin with any trailing slash trimmed (e.g. `https://host`).
    pub server: String,
    /// API version path segment (e.g. `v1`).
    pub api_version: String,
    /// Bearer token, if one could be sourced. `None` is valid for `login`
    /// (which acquires a token); entity verbs surface a clear error.
    pub token: Option<String>,
    /// PEM CA certificate path for a private-PKI server certificate.
    pub ca_cert: Option<PathBuf>,
    /// Skip TLS verification (INSECURE).
    pub insecure: bool,
    /// Per-request timeout.
    pub timeout: Duration,
}

impl std::fmt::Debug for ResolvedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedClient")
            .field("server", &self.server)
            .field("api_version", &self.api_version)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("ca_cert", &self.ca_cert)
            .field("insecure", &self.insecure)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Resolve entity-command connection settings from, in precedence order:
/// CLI flags, environment variables, the `[schema_forge.client]` config
/// section, and built-in defaults.
///
/// Performs IO: reads the token from stdin/file/cache as directed. The token
/// is never logged.
pub fn resolve_client_config(
    svc: &Config<SchemaForgeConfig>,
    conn: &EntityConnectionArgs,
) -> Result<ResolvedClient, CliError> {
    let client_cfg = &svc.custom.schema_forge.client;

    let server = conn
        .server
        .clone()
        .or_else(|| client_cfg.server.clone())
        .unwrap_or_else(|| DEFAULT_CLIENT_SERVER.to_string());
    let server = server.trim_end_matches('/').to_string();

    let timeout_secs = conn
        .timeout
        .or(client_cfg.timeout_secs)
        .unwrap_or(DEFAULT_CLIENT_TIMEOUT_SECS);

    let ca_cert = conn
        .ca_cert
        .clone()
        .or_else(|| client_cfg.ca_cert.clone().map(PathBuf::from));

    let token = resolve_token(conn, client_cfg)?;

    Ok(ResolvedClient {
        server,
        api_version: conn.api_version.clone(),
        token,
        ca_cert,
        insecure: conn.insecure,
        timeout: Duration::from_secs(timeout_secs),
    })
}

/// Source the Bearer token, highest precedence first:
/// 1. `--token-stdin`, 2. `--token-file`, 3. `SCHEMAFORGE_TOKEN` env,
/// 4. `[schema_forge.client] token_file`, 5. the cached `login` token.
fn resolve_token(
    conn: &EntityConnectionArgs,
    client_cfg: &ClientConfig,
) -> Result<Option<String>, CliError> {
    if conn.token_stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::Io {
                path: PathBuf::from("<stdin>"),
                source: e,
            })?;
        return Ok(Some(buf.trim().to_string()));
    }
    if let Some(path) = &conn.token_file {
        return Ok(Some(read_token_file(path)?));
    }
    if let Ok(tok) = std::env::var("SCHEMAFORGE_TOKEN") {
        if !tok.trim().is_empty() {
            return Ok(Some(tok.trim().to_string()));
        }
    }
    if let Some(path) = &client_cfg.token_file {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(Some(read_token_file(&p)?));
        }
    }
    let cached = xdg_state_token_path();
    if cached.exists() {
        return Ok(Some(read_token_file(&cached)?));
    }
    Ok(None)
}

fn read_token_file(path: &Path) -> Result<String, CliError> {
    let text = std::fs::read_to_string(path).map_err(|e| CliError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(text.trim().to_string())
}

/// Path to the cached `login` token: `$XDG_STATE_HOME/schemaforge/token`,
/// falling back to `~/.local/state/schemaforge/token`.
pub fn xdg_state_token_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("schemaforge").join("token");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("schemaforge")
        .join("token")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_global() -> GlobalOpts {
        GlobalOpts {
            config: None,
            format: "human".into(),
            verbose: 0,
            quiet: false,
            no_color: false,
            db_url: None,
            db_ns: None,
            db_name: None,
            trust_policy: None,
            no_verify: false,
        }
    }

    #[test]
    fn is_postgres_url_recognizes_both_schemes() {
        assert!(is_postgres_url("postgres://user:pass@host/db"));
        assert!(is_postgres_url("postgresql://localhost/db"));
        assert!(!is_postgres_url("ws://localhost:8000"));
        assert!(!is_postgres_url("mem://"));
    }

    #[test]
    fn no_config_no_cli_falls_back_to_dev_surrealdb() {
        let svc: Config<SchemaForgeConfig> = Config::default();
        let params = resolve_db_params(&svc).unwrap();
        match params {
            DbParams::Surrealdb(p) => {
                assert_eq!(p.url, DEFAULT_DEV_SURREALDB_URL);
                assert_eq!(p.namespace, DEFAULT_SURREALDB_NAMESPACE);
                assert_eq!(p.database, DEFAULT_SURREALDB_DATABASE);
            }
            DbParams::Postgres(_) | DbParams::Mssql(_) => panic!("expected SurrealDB default"),
        }
    }

    fn pg_section(url: &str) -> acton_service::config::DatabaseConfig {
        toml::from_str(&format!("url = {}", toml::Value::String(url.to_string()))).unwrap()
    }

    #[cfg(feature = "surrealdb")]
    fn surreal_section(url: &str) -> acton_service::config::SurrealDbConfig {
        acton_service::config::SurrealDbConfig {
            url: url.to_string(),
            namespace: "ns".into(),
            database: "db".into(),
            username: None,
            password: None,
            max_retries: 5,
            retry_delay_secs: 2,
            optional: false,
            lazy_init: true,
        }
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn surrealdb_section_round_trips_through_resolver() {
        let svc = Config::<SchemaForgeConfig> {
            surrealdb: Some(acton_service::config::SurrealDbConfig {
                url: "ws://prod:8000".into(),
                namespace: "ns".into(),
                database: "db".into(),
                username: Some("admin".into()),
                password: Some("secret".into()),
                max_retries: 5,
                retry_delay_secs: 2,
                optional: false,
                lazy_init: true,
            }),
            ..Config::default()
        };
        let DbParams::Surrealdb(p) = resolve_db_params(&svc).unwrap() else {
            panic!("expected SurrealDB");
        };
        assert_eq!(p.url, "ws://prod:8000");
        assert_eq!(p.namespace, "ns");
        assert_eq!(p.database, "db");
        assert_eq!(p.username.as_deref(), Some("admin"));
        assert_eq!(p.password.as_deref(), Some("secret"));
    }

    #[test]
    fn database_section_round_trips_through_resolver() {
        let svc = Config::<SchemaForgeConfig> {
            database: Some(pg_section("postgres://u:p@h/db")),
            ..Config::default()
        };
        let DbParams::Postgres(p) = resolve_db_params(&svc).unwrap() else {
            panic!("expected Postgres");
        };
        assert_eq!(p.url, "postgres://u:p@h/db");
    }

    #[test]
    fn mssql_connection_strings_are_detected_case_insensitively() {
        assert!(is_mssql_connection_string("Server=sql01;Database=forge"));
        assert!(is_mssql_connection_string(
            "DATA SOURCE=sql01;Initial Catalog=forge"
        ));
        assert!(!is_mssql_connection_string("postgres://sql01/forge"));
    }

    #[cfg(feature = "mssql")]
    #[test]
    fn mssql_integrated_auth_round_trips_through_resolver() {
        use acton_service::config::MssqlAuthMode;

        let database = toml::from_str(
            "url = 'Server=sql01;Database=forge;TrustServerCertificate=true'\n\
             mssql_auth = 'integrated'",
        )
        .unwrap();
        let svc = Config::<SchemaForgeConfig> {
            database: Some(database),
            ..Config::default()
        };
        let DbParams::Mssql(params) = resolve_db_params(&svc).unwrap() else {
            panic!("expected SQL Server");
        };
        assert_eq!(params.config.mssql_auth, MssqlAuthMode::Integrated);
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn ambiguous_dual_section_is_an_error() {
        let svc = Config::<SchemaForgeConfig> {
            database: Some(pg_section("postgres://h/db")),
            surrealdb: Some(surreal_section("ws://h:8000")),
            ..Config::default()
        };
        assert!(resolve_db_params(&svc).is_err());
    }

    /// Issue #47: `--db-url postgres://X` must override a config-file
    /// `[database] url = "postgres://Y"`. Before the refactor, schema-forge
    /// kept its own `[database]` copy that the CLI flag patched while
    /// acton-service's copy stayed at Y, producing two pools to two DBs.
    #[test]
    fn cli_db_url_overrides_database_section_issue_47() {
        let mut svc = Config::<SchemaForgeConfig> {
            database: Some(
                toml::from_str(
                    "url = 'postgres://stale@config-host:5433/db'\n\
                 max_connections = 42\nmin_connections = 7\n\
                 connection_timeout_secs = 11\nmax_retries = 9\nretry_delay_secs = 3",
                )
                .unwrap(),
            ),
            ..Config::default()
        };

        let global = GlobalOpts {
            db_url: Some("postgres://right@cli-host:5432/db".into()),
            ..empty_global()
        };

        apply_cli_overrides(&mut svc, &global).unwrap();

        let db = svc.database.as_ref().unwrap();
        assert_eq!(db.url, "postgres://right@cli-host:5432/db");
        // Pool-sizing knobs the operator set in config.toml must survive
        // the URL override — we are fixing precedence, not clobbering
        // tunables.
        assert_eq!(db.max_connections, 42);
        assert_eq!(db.min_connections, 7);
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn cli_db_url_clears_other_backend_section() {
        // Switching backends on the command line must also clear the
        // not-selected section so acton-service doesn't spawn an extra
        // pool from leftover config.
        let mut svc = Config::<SchemaForgeConfig> {
            surrealdb: Some(surreal_section("ws://leftover:8000")),
            ..Config::default()
        };

        let global = GlobalOpts {
            db_url: Some("postgres://h/db".into()),
            ..empty_global()
        };
        apply_cli_overrides(&mut svc, &global).unwrap();

        assert!(svc.surrealdb.is_none());
        assert_eq!(svc.database.as_ref().unwrap().url, "postgres://h/db");
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn cli_db_url_creates_database_section_when_absent() {
        let mut svc: Config<SchemaForgeConfig> = Config::default();
        let global = GlobalOpts {
            db_url: Some("postgres://x@host/db".into()),
            ..empty_global()
        };
        apply_cli_overrides(&mut svc, &global).unwrap();
        let db = svc.database.as_ref().expect("section must be created");
        assert_eq!(db.url, "postgres://x@host/db");
        // Defaults inherited from acton-service's serde annotations.
        assert_eq!(db.max_connections, 50);
        assert_eq!(db.min_connections, 5);
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn cli_db_url_creates_surrealdb_section_when_absent() {
        let mut svc: Config<SchemaForgeConfig> = Config::default();
        let global = GlobalOpts {
            db_url: Some("ws://localhost:9000".into()),
            ..empty_global()
        };
        apply_cli_overrides(&mut svc, &global).unwrap();
        let s = svc.surrealdb.as_ref().expect("section must be created");
        assert_eq!(s.url, "ws://localhost:9000");
    }

    #[test]
    fn build_verify_policy_off_by_default() {
        let svc: Config<SchemaForgeConfig> = Config::default();
        let policy = build_verify_policy(&svc, &empty_global()).unwrap();
        assert_eq!(policy.mode(), SigningMode::Off);
    }

    #[test]
    fn build_verify_policy_honours_no_verify_in_warn_mode() {
        let mut svc: Config<SchemaForgeConfig> = Config::default();
        svc.custom.schema_forge.signing.mode = SigningMode::Warn;
        let mut global = empty_global();
        global.no_verify = true;
        let policy = build_verify_policy(&svc, &global).unwrap();
        assert_eq!(policy.mode(), SigningMode::Off);
    }

    #[test]
    fn build_verify_policy_refuses_no_verify_under_enforce() {
        std::env::remove_var("SCHEMAFORGE_ALLOW_NO_VERIFY");
        let mut svc: Config<SchemaForgeConfig> = Config::default();
        svc.custom.schema_forge.signing.mode = SigningMode::Enforce;
        svc.custom.schema_forge.signing.trusted_signers =
            vec![schema_forge_signing::TrustedSigner::Ed25519 {
                name: "x".into(),
                public_key_b64: schema_forge_signing::Ed25519Signer::from_seed_bytes(&[0x42; 32])
                    .public_key_b64_raw(),
            }];
        let mut global = empty_global();
        global.no_verify = true;
        let err = build_verify_policy(&svc, &global).unwrap_err();
        assert!(matches!(err, CliError::Config { .. }));
    }

    #[test]
    fn build_verify_policy_allows_no_verify_with_env_override() {
        std::env::set_var("SCHEMAFORGE_ALLOW_NO_VERIFY", "1");
        let mut svc: Config<SchemaForgeConfig> = Config::default();
        svc.custom.schema_forge.signing.mode = SigningMode::Enforce;
        svc.custom.schema_forge.signing.trusted_signers =
            vec![schema_forge_signing::TrustedSigner::Ed25519 {
                name: "x".into(),
                public_key_b64: schema_forge_signing::Ed25519Signer::from_seed_bytes(&[0x42; 32])
                    .public_key_b64_raw(),
            }];
        let mut global = empty_global();
        global.no_verify = true;
        let policy = build_verify_policy(&svc, &global).unwrap();
        assert_eq!(policy.mode(), SigningMode::Off);
        std::env::remove_var("SCHEMAFORGE_ALLOW_NO_VERIFY");
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn cli_ns_and_name_override_surrealdb_section() {
        let mut svc = Config::<SchemaForgeConfig> {
            surrealdb: Some(acton_service::config::SurrealDbConfig {
                url: "ws://h:8000".into(),
                namespace: "from_config".into(),
                database: "from_config".into(),
                username: None,
                password: None,
                max_retries: 5,
                retry_delay_secs: 2,
                optional: false,
                lazy_init: true,
            }),
            ..Config::default()
        };
        let global = GlobalOpts {
            db_ns: Some("cli_ns".into()),
            db_name: Some("cli_db".into()),
            ..empty_global()
        };
        apply_cli_overrides(&mut svc, &global).unwrap();
        let s = svc.surrealdb.as_ref().unwrap();
        assert_eq!(s.namespace, "cli_ns");
        assert_eq!(s.database, "cli_db");
    }
}
