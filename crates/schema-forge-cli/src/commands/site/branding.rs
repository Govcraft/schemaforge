//! Resolve product identity independently of output directory names.

use std::path::Path;

use heck::{ToKebabCase, ToTitleCase};
use schema_forge_acton::config::SiteBrandingConfig;
use schema_forge_core::types::{Annotation, SchemaDefinition, TenantKind};
use serde::Serialize;

use crate::cli::{GlobalOpts, SiteGenerateArgs};
use crate::error::CliError;

#[derive(Debug, Clone, Serialize)]
pub struct Branding {
    pub name: String,
    pub name_json: String,
    pub title_suffix_json: String,
    pub theme_key_json: String,
    pub logo: Option<String>,
    pub logo_on_dark: Option<String>,
    pub favicon: Option<String>,
}

impl Branding {
    pub fn resolve(
        args: &SiteGenerateArgs,
        config: &SiteBrandingConfig,
        global: &GlobalOpts,
        schemas: &[SchemaDefinition],
    ) -> Result<Self, CliError> {
        let name = args
            .name
            .clone()
            .or_else(|| config.name.clone())
            .unwrap_or_else(|| {
                schemas
                    .iter()
                    .find(|schema| {
                        schema.annotations.iter().any(|annotation| {
                            matches!(annotation, Annotation::Tenant(TenantKind::Root))
                        })
                    })
                    .map(|schema| schema.name.as_str().to_title_case())
                    .or_else(|| {
                        args.schema_dir.canonicalize().ok().and_then(|path| {
                            path.parent()
                                .and_then(Path::file_name)
                                .map(|name| name.to_string_lossy().to_title_case())
                        })
                    })
                    .unwrap_or_else(|| "Application".into())
            });
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(CliError::Config {
                message: "site name must be nonempty and contain no control characters".into(),
            });
        }
        let title_suffix = args
            .title_suffix
            .as_ref()
            .or(config.title_suffix.as_ref())
            .unwrap_or(&name);
        let config_base = global
            .config
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."));
        let logo = read_asset(args.logo.as_deref(), config.logo.as_deref(), config_base)?;
        let logo_on_dark = read_asset(
            args.logo_on_dark.as_deref(),
            config.logo_on_dark.as_deref(),
            config_base,
        )?
        .or_else(|| logo.clone());
        let favicon = read_asset(
            args.favicon.as_deref(),
            config.favicon.as_deref(),
            config_base,
        )?
        .or_else(|| logo.clone());
        let theme_key = format!("{}.theme", name.to_kebab_case());
        Ok(Self {
            name_json: json_string(&name)?,
            title_suffix_json: json_string(title_suffix)?,
            theme_key_json: json_string(&theme_key)?,
            name,
            logo,
            logo_on_dark,
            favicon,
        })
    }
}

fn json_string(value: &str) -> Result<String, CliError> {
    serde_json::to_string(value).map_err(|error| CliError::Config {
        message: format!("failed to encode site branding: {error}"),
    })
}

fn read_asset(
    flag: Option<&Path>,
    configured: Option<&Path>,
    config_base: &Path,
) -> Result<Option<String>, CliError> {
    let Some(path) = flag
        .map(Path::to_path_buf)
        .or_else(|| configured.map(|path| config_base.join(path)))
    else {
        return Ok(None);
    };
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        return Err(CliError::Config {
            message: format!("site branding asset {} must be an SVG file", path.display()),
        });
    }
    let contents =
        std::fs::read_to_string(&path).map_err(|source| CliError::Io { path, source })?;
    if !contents.contains("<svg") {
        return Err(CliError::Config {
            message: "site branding asset must contain an SVG document".into(),
        });
    }
    Ok(Some(contents))
}
