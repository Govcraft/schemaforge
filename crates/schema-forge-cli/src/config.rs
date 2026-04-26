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

use std::path::Path;

use acton_service::config::Config;
use schema_forge_acton::SchemaForgeConfig;

use crate::cli::GlobalOpts;
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

/// Resolved backend connection parameters. The variant is selected by URL
/// scheme; see [`is_postgres_url`].
#[derive(Debug, Clone)]
pub enum DbParams {
    Surrealdb(SurrealDbParams),
    Postgres(PostgresParams),
}

impl DbParams {
    /// The connection URL, regardless of backend.
    pub fn url(&self) -> &str {
        match self {
            DbParams::Surrealdb(p) => &p.url,
            DbParams::Postgres(p) => &p.url,
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
        if is_postgres_url(cli_url) {
            apply_cli_postgres_url(svc, cli_url);
        } else {
            apply_cli_surrealdb_url(svc, cli_url);
        }
    }

    apply_cli_surrealdb_naming(svc, global)?;
    Ok(())
}

fn apply_cli_postgres_url(svc: &mut Config<SchemaForgeConfig>, url: &str) {
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
/// - `[database]` present → PostgreSQL.
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

/// Detect PostgreSQL URLs by scheme.
pub fn is_postgres_url(url: &str) -> bool {
    url.starts_with("postgres://") || url.starts_with("postgresql://")
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
            DbParams::Postgres(_) => panic!("expected SurrealDB default"),
        }
    }

    fn pg_section(url: &str) -> acton_service::config::DatabaseConfig {
        acton_service::config::DatabaseConfig {
            url: url.to_string(),
            max_connections: 50,
            min_connections: 5,
            connection_timeout_secs: 10,
            max_retries: 3,
            retry_delay_secs: 2,
            optional: false,
            lazy_init: true,
        }
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
            database: Some(acton_service::config::DatabaseConfig {
                url: "postgres://stale@config-host:5433/db".into(),
                max_connections: 42,
                min_connections: 7,
                connection_timeout_secs: 11,
                max_retries: 9,
                retry_delay_secs: 3,
                optional: false,
                lazy_init: true,
            }),
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
