use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::GlobalOpts;
use crate::error::CliError;

/// CLI configuration loaded from config.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliConfig {
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub cli: CliSettings,
}

/// Database connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_url")]
    pub url: String,
    #[serde(default = "default_db_ns")]
    pub namespace: String,
    #[serde(default = "default_db_name")]
    pub database: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_db_url(),
            namespace: default_db_ns(),
            database: default_db_name(),
        }
    }
}

/// CLI-specific settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliSettings {
    #[serde(default = "default_schema_dir")]
    pub default_schema_dir: String,
    #[serde(default = "default_policy_dir")]
    pub default_policy_dir: String,
}

impl Default for CliSettings {
    fn default() -> Self {
        Self {
            default_schema_dir: default_schema_dir(),
            default_policy_dir: default_policy_dir(),
        }
    }
}

fn default_db_url() -> String {
    "ws://localhost:8000".to_string()
}

fn default_db_ns() -> String {
    "schemaforge".to_string()
}

fn default_db_name() -> String {
    "dev".to_string()
}

fn default_schema_dir() -> String {
    "schemas/".to_string()
}

fn default_policy_dir() -> String {
    "policies/".to_string()
}

/// SurrealDB-specific connection parameters.
#[derive(Debug, Clone)]
pub struct SurrealDbParams {
    pub url: String,
    pub namespace: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// PostgreSQL-specific connection parameters.
#[derive(Debug, Clone)]
pub struct PostgresParams {
    pub url: String,
}

/// Resolved database connection parameters after merging config + CLI flags.
///
/// Each variant carries only the fields relevant to that backend.
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

/// Discovery order for config file:
/// 1. `--config <path>` (explicit)
/// 2. `SCHEMA_FORGE_CONFIG` env var
/// 3. `./config.toml` (project-local)
/// 4. `$XDG_CONFIG_HOME/schema-forge/config.toml`
/// 5. `~/.config/schema-forge/config.toml`
pub fn load_config(explicit_path: Option<&Path>) -> Result<CliConfig, CliError> {
    if let Some(path) = explicit_path {
        return load_config_from_path(path);
    }

    if let Ok(env_path) = std::env::var("SCHEMA_FORGE_CONFIG") {
        let path = PathBuf::from(env_path);
        if path.exists() {
            return load_config_from_path(&path);
        }
    }

    let local = PathBuf::from("config.toml");
    if local.exists() {
        return load_config_from_path(&local);
    }

    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(xdg).join("schema-forge/config.toml");
        if path.exists() {
            return load_config_from_path(&path);
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home).join(".config/schema-forge/config.toml");
        if path.exists() {
            return load_config_from_path(&path);
        }
    }

    // No config file found; use defaults.
    Ok(CliConfig::default())
}

fn load_config_from_path(path: &Path) -> Result<CliConfig, CliError> {
    let contents = std::fs::read_to_string(path).map_err(|e| CliError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    toml::from_str(&contents).map_err(|e| CliError::Config {
        message: format!("failed to parse {}: {}", path.display(), e),
    })
}

/// Resolve database connection parameters from config + CLI overrides.
///
/// CLI flags take precedence over config file values.
/// The backend is auto-detected from the URL scheme:
/// - `postgres://` or `postgresql://` → PostgreSQL
/// - Anything else (ws://, wss://, mem://, http://, https://) → SurrealDB
pub fn resolve_db_params(config: &CliConfig, global: &GlobalOpts) -> DbParams {
    let url = global
        .db_url
        .clone()
        .unwrap_or_else(|| config.database.url.clone());

    if is_postgres_url(&url) {
        return DbParams::Postgres(PostgresParams { url });
    }

    DbParams::Surrealdb(SurrealDbParams {
        url,
        namespace: global
            .db_ns
            .clone()
            .unwrap_or_else(|| config.database.namespace.clone()),
        database: global
            .db_name
            .clone()
            .unwrap_or_else(|| config.database.database.clone()),
        username: std::env::var("SCHEMA_FORGE_DB_USER").ok(),
        password: std::env::var("SCHEMA_FORGE_DB_PASS").ok(),
    })
}

/// Detect PostgreSQL URLs by scheme.
fn is_postgres_url(url: &str) -> bool {
    url.starts_with("postgres://") || url.starts_with("postgresql://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let config = CliConfig::default();
        assert_eq!(config.database.url, "ws://localhost:8000");
        assert_eq!(config.database.namespace, "schemaforge");
        assert_eq!(config.database.database, "dev");
        assert_eq!(config.cli.default_schema_dir, "schemas/");
        assert_eq!(config.cli.default_policy_dir, "policies/");
    }

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"
[database]
url = "ws://custom:9000"
"#;
        let config: CliConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.database.url, "ws://custom:9000");
        // Defaults for missing fields
        assert_eq!(config.database.namespace, "schemaforge");
        assert_eq!(config.database.database, "dev");
    }

    #[test]
    fn parse_full_toml() {
        let toml_str = r#"
[database]
url = "ws://prod:8000"
namespace = "production"
database = "main"

[cli]
default_schema_dir = "src/schemas/"
default_policy_dir = "src/policies/"
"#;
        let config: CliConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.database.url, "ws://prod:8000");
        assert_eq!(config.database.namespace, "production");
        assert_eq!(config.database.database, "main");
        assert_eq!(config.cli.default_schema_dir, "src/schemas/");
        assert_eq!(config.cli.default_policy_dir, "src/policies/");
    }

    #[test]
    fn resolve_db_params_uses_config_defaults() {
        let config = CliConfig::default();
        let global = GlobalOpts {
            config: None,
            format: "human".into(),
            verbose: 0,
            quiet: false,
            no_color: false,
            db_url: None,
            db_ns: None,
            db_name: None,
        };
        let params = resolve_db_params(&config, &global);
        assert!(matches!(params, DbParams::Surrealdb(_)));
        assert_eq!(params.url(), "ws://localhost:8000");
        if let DbParams::Surrealdb(p) = &params {
            assert_eq!(p.namespace, "schemaforge");
            assert_eq!(p.database, "dev");
        }
    }

    #[test]
    fn resolve_db_params_cli_overrides() {
        let config = CliConfig::default();
        let global = GlobalOpts {
            config: None,
            format: "human".into(),
            verbose: 0,
            quiet: false,
            no_color: false,
            db_url: Some("ws://override:9999".into()),
            db_ns: Some("custom_ns".into()),
            db_name: Some("custom_db".into()),
        };
        let params = resolve_db_params(&config, &global);
        assert!(matches!(params, DbParams::Surrealdb(_)));
        assert_eq!(params.url(), "ws://override:9999");
        if let DbParams::Surrealdb(p) = &params {
            assert_eq!(p.namespace, "custom_ns");
            assert_eq!(p.database, "custom_db");
        }
    }

    #[test]
    fn resolve_db_params_detects_postgres_url() {
        let config = CliConfig::default();
        let global = GlobalOpts {
            config: None,
            format: "human".into(),
            verbose: 0,
            quiet: false,
            no_color: false,
            db_url: Some("postgres://user:pass@localhost/mydb".into()),
            db_ns: None,
            db_name: None,
        };
        let params = resolve_db_params(&config, &global);
        assert!(matches!(params, DbParams::Postgres(_)));
        assert_eq!(params.url(), "postgres://user:pass@localhost/mydb");
    }

    #[test]
    fn resolve_db_params_detects_postgresql_url() {
        let config = CliConfig::default();
        let global = GlobalOpts {
            config: None,
            format: "human".into(),
            verbose: 0,
            quiet: false,
            no_color: false,
            db_url: Some("postgresql://localhost/mydb".into()),
            db_ns: None,
            db_name: None,
        };
        let params = resolve_db_params(&config, &global);
        assert!(matches!(params, DbParams::Postgres(_)));
    }

    #[test]
    fn load_config_returns_default_when_no_file() {
        // In a temp dir with no config.toml, should return defaults
        let config = load_config(None).unwrap();
        assert_eq!(config.database.url, "ws://localhost:8000");
    }

    #[test]
    fn load_config_from_explicit_missing_file() {
        let result = load_config(Some(Path::new("/nonexistent/config.toml")));
        assert!(result.is_err());
    }
}
