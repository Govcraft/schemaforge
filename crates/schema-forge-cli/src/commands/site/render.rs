//! minijinja-backed template rendering for the site generator.
//!
//! Templates live in `crates/schema-forge-cli/templates/site/` and are baked
//! into the binary at compile time via `include_str!`. The logical template
//! name is the final relative output path (e.g. `"src/App.tsx"`), which makes
//! the `build_plan` → `renderer.render` call sites self-documenting.

use minijinja::Environment;
use serde::Serialize;

use crate::error::CliError;

/// Thin wrapper around a preloaded minijinja [`Environment`].
pub struct SiteRenderer {
    env: Environment<'static>,
}

impl SiteRenderer {
    /// Build a fresh renderer with every site template registered.
    pub fn new() -> Result<Self, CliError> {
        let mut env = Environment::new();

        macro_rules! add {
            ($name:expr, $path:expr) => {
                env.add_template($name, include_str!($path))
                    .map_err(|e| CliError::Config {
                        message: format!("site template `{}` failed to load: {e}", $name),
                    })?;
            };
        }

        add!("package.json", "../../../templates/site/package.json.jinja");
        add!("index.html", "../../../templates/site/index.html.jinja");
        add!("vite.config.ts", "../../../templates/site/vite.config.ts.jinja");
        add!(
            "tailwind.config.ts",
            "../../../templates/site/tailwind.config.ts.jinja"
        );
        add!("src/main.tsx", "../../../templates/site/src/main.tsx.jinja");
        add!("src/App.tsx", "../../../templates/site/src/App.tsx.jinja");
        add!(
            "src/generated/api-client.ts",
            "../../../templates/site/src/generated/api-client.ts.jinja"
        );
        add!(
            "src/generated/entity-types.ts",
            "../../../templates/site/src/generated/entity-types.ts.jinja"
        );
        add!(
            "src/generated/zod-schemas.ts",
            "../../../templates/site/src/generated/zod-schemas.ts.jinja"
        );
        add!(
            "src/generated/route-manifest.ts",
            "../../../templates/site/src/generated/route-manifest.ts.jinja"
        );
        add!(
            "src/generated/formatters.ts",
            "../../../templates/site/src/generated/formatters.ts.jinja"
        );
        add!(
            "src/lib/auth.ts",
            "../../../templates/site/src/lib/auth.ts.jinja"
        );
        add!(
            "src/lib/require-auth.tsx",
            "../../../templates/site/src/lib/require-auth.tsx.jinja"
        );
        add!(
            "src/pages/login.tsx",
            "../../../templates/site/src/pages/login.tsx.jinja"
        );
        add!(
            "src/pages/list.tsx",
            "../../../templates/site/src/pages/list.tsx.jinja"
        );
        add!(
            "src/pages/detail.tsx",
            "../../../templates/site/src/pages/detail.tsx.jinja"
        );
        add!(
            "src/pages/edit.tsx",
            "../../../templates/site/src/pages/edit.tsx.jinja"
        );

        Ok(Self { env })
    }

    /// Render a registered template against `ctx`, producing the final file
    /// contents (without a marker header — that is added by the codegen
    /// write layer downstream).
    pub fn render<C: Serialize>(&self, name: &str, ctx: &C) -> Result<String, CliError> {
        let tmpl = self
            .env
            .get_template(name)
            .map_err(|e| CliError::Config {
                message: format!("site template `{name}` not registered: {e}"),
            })?;
        tmpl.render(ctx).map_err(|e| CliError::Config {
            message: format!("site template `{name}` failed to render: {e}"),
        })
    }
}
