//! Lightweight config loader for offline schema and generation commands.
use crate::cli::GlobalOpts;
use crate::error::CliError;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use schema_forge_config::SiteBrandingConfig;
use schema_forge_signing::{SigningConfig, SigningMode, VerifyPolicy};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ToolingConfig {
    pub custom: ToolingCustom,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ToolingCustom {
    pub schema_forge: ToolingSettings,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolingSettings {
    pub site: SiteBrandingConfig,
    pub signing: SigningConfig,
}

pub fn load_svc_config(global: &GlobalOpts) -> Result<ToolingConfig, CliError> {
    // Framework custom settings are flattened at the TOML root.
    let mut root = Figment::from(Serialized::defaults(ToolingCustom::default()));
    for path in service_config_paths(global.config.as_deref()).iter().rev() {
        if path.exists() {
            root = root.merge(Toml::file(path));
        }
    }
    if let Some(path) = global.config.as_ref().filter(|path| !path.is_file()) {
        return Err(CliError::Io {
            path: path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "configuration file not found",
            ),
        });
    }
    let custom = root
        .merge(
            Env::prefixed("ACTON_")
                .filter(|key| !key.as_str().contains("__"))
                .split("_"),
        )
        .merge(
            Env::prefixed("ACTON_")
                .filter(|key| key.as_str().contains("__"))
                .split("__"),
        )
        .extract()
        .map_err(|error| CliError::Config {
            message: format!("failed to load configuration: {error}"),
        })?;
    Ok(ToolingConfig { custom })
}

/// Match acton-service's user config discovery, without recommended_path's
/// advisory relative fallback (which the framework does not actually load).
fn user_service_config_path() -> Option<PathBuf> {
    #[cfg(unix)]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    #[cfg(windows)]
    let root = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
    });
    #[cfg(not(any(unix, windows)))]
    let root: Option<PathBuf> = None;
    root.map(|root| {
        root.join("acton-service")
            .join("schemaforge")
            .join("config.toml")
    })
}

fn service_config_paths(explicit: Option<&Path>) -> Vec<PathBuf> {
    if let Some(path) = explicit {
        return vec![path.to_path_buf()];
    }
    let mut paths = vec![PathBuf::from("config.toml")];
    if let Some(path) = user_service_config_path().filter(|path| path.is_file()) {
        paths.push(path);
    }
    #[cfg(unix)]
    paths.push(PathBuf::from("/etc/acton-service/schemaforge/config.toml"));
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("PROGRAMDATA") {
        paths.push(PathBuf::from(root).join("acton-service/schemaforge/config.toml"));
    }
    paths
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
///    already-loaded `ToolingConfig`.
///
/// Refuses to honour `--no-verify` when the resolved policy is
/// `mode = "enforce"`. Operators who want a working escape hatch in
/// strict environments must instead set `SCHEMAFORGE_ALLOW_NO_VERIFY=1`
/// — the env var is the explicit acknowledgement that they own the
/// risk for this command. This matches the design used for
/// production-grade auth bypasses.
pub fn build_verify_policy(
    svc_config: &ToolingConfig,
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
    svc_config: &ToolingConfig,
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
