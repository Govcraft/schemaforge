//! minijinja-backed template rendering for the site generator.
//!
//! Templates are registered via a filesystem-first loader:
//!
//!   1. If an override directory was passed, look for `<dir>/<name>.jinja`
//!      on disk and use it if present.
//!   2. Fall back to the slice baked into the binary at build time
//!      (`EMBEDDED_SITE_TEMPLATES`, emitted by `build.rs`).
//!
//! This lets framework users iterate on generator output without a CLI
//! rebuild: drop an override tree next to your schemas, tweak `.jinja`
//! files, re-run `schema-forge site generate`, and only the overridden
//! files swap — every other template still comes from the embedded
//! defaults. When the overrides look right, copy them back into
//! `crates/schema-forge-cli/templates/site/` and they become the new
//! baked-in default.
//!
//! Logical template names (`"src/App.tsx"`, `"package.json"`, …) are the
//! post-`.jinja`-strip relative paths, which is also the final output
//! path — so call sites read naturally.

use std::path::PathBuf;

use minijinja::Environment;
use serde::Serialize;

use crate::error::CliError;

include!(concat!(env!("OUT_DIR"), "/embedded_site_templates.rs"));

/// Thin wrapper around a preloaded minijinja [`Environment`].
pub struct SiteRenderer {
    env: Environment<'static>,
}

impl SiteRenderer {
    /// Build a fresh renderer.
    ///
    /// If `override_dir` is `Some`, the loader checks that directory for
    /// `<logical_name>.jinja` before falling back to the embedded defaults.
    /// Read errors on an override file are treated as "not overridden" and
    /// silently fall through to the embedded template.
    pub fn new(override_dir: Option<PathBuf>) -> Result<Self, CliError> {
        let mut env = Environment::new();
        env.set_loader(move |name: &str| {
            if let Some(ref dir) = override_dir {
                let candidate = dir.join(format!("{name}.jinja"));
                if candidate.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&candidate) {
                        return Ok(Some(content));
                    }
                }
            }
            for (logical, content) in EMBEDDED_SITE_TEMPLATES {
                if *logical == name {
                    return Ok(Some((*content).to_string()));
                }
            }
            Ok(None)
        });
        Ok(Self { env })
    }

    /// Render a registered template against `ctx`, producing the final file
    /// contents (without a marker header — that is added by the codegen
    /// write layer downstream).
    pub fn render<C: Serialize>(&self, name: &str, ctx: &C) -> Result<String, CliError> {
        let tmpl = self.env.get_template(name).map_err(|e| CliError::Config {
            message: format!("site template `{name}` not registered: {e}"),
        })?;
        tmpl.render(ctx).map_err(|e| CliError::Config {
            message: format!("site template `{name}` failed to render: {e}"),
        })
    }
}
