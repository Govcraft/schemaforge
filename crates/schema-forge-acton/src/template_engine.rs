use std::path::PathBuf;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::Environment;
use serde::Serialize;

/// MiniJinja-based template engine with filesystem-only loading.
///
/// Templates are loaded from a required directory path. No embedded templates
/// are compiled into the binary — all templates live on the filesystem.
pub struct TemplateEngine {
    env: Environment<'static>,
}

impl TemplateEngine {
    /// Create a new engine loading templates from the given directory.
    pub fn new(template_dir: PathBuf) -> Self {
        let mut env = Environment::new();

        env.set_loader(move |name| {
            let path = template_dir.join(name);
            if path.is_file() {
                match std::fs::read_to_string(&path) {
                    Ok(content) => Ok(Some(content)),
                    Err(_) => Ok(None),
                }
            } else {
                Ok(None)
            }
        });

        // Register custom `split` filter for `field.value | split(", ")`
        env.add_filter("split", |value: &str, sep: &str| -> Vec<String> {
            value.split(sep).map(|s| s.to_string()).collect()
        });

        // Register custom `truncate` filter: `value | truncate(length=N, end="...")`
        env.add_filter(
            "truncate",
            |value: &str, kwargs: minijinja::value::Kwargs| -> Result<String, minijinja::Error> {
                let length: usize = kwargs.get("length").unwrap_or(255);
                let end: String = kwargs.get("end").unwrap_or_else(|_| "...".to_string());
                kwargs.assert_all_used()?;
                if value.len() <= length {
                    Ok(value.to_string())
                } else {
                    let truncated: String = value.chars().take(length).collect();
                    Ok(format!("{truncated}{end}"))
                }
            },
        );

        Self { env }
    }

    /// Render a template by name with a serializable context.
    pub fn render<T: Serialize>(&self, name: &str, ctx: &T) -> Result<String, String> {
        let tmpl = self.env.get_template(name).map_err(|e| e.to_string())?;
        tmpl.render(ctx).map_err(|e| e.to_string())
    }
}

/// Render a full-page template (Content-Type: text/html).
pub fn render_template<T: Serialize>(engine: &TemplateEngine, name: &str, ctx: &T) -> Response {
    match engine.render(name, ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {e}"),
        )
            .into_response(),
    }
}

/// Render a template fragment (Content-Type: text/html, no layout wrapping).
/// Same as render_template but semantically for HTMX fragments.
pub fn render_fragment<T: Serialize>(engine: &TemplateEngine, name: &str, ctx: &T) -> Response {
    render_template(engine, name, ctx)
}

/// Render a template with a custom HTTP status code.
pub fn render_template_with_status<T: Serialize>(
    engine: &TemplateEngine,
    name: &str,
    ctx: &T,
    status: StatusCode,
) -> Response {
    match engine.render(name, ctx) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_template_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
    }

    #[test]
    fn all_filesystem_templates_loadable() {
        let engine = TemplateEngine::new(test_template_dir());
        // Spot-check that a selection of templates can be loaded from disk
        let names = [
            "admin/base.html",
            "admin/login.html",
            "admin/dashboard.html",
            "admin/entity_list.html",
            "admin/entity_form.html",
            "admin/entity_detail.html",
            "shared/atoms/field_display.html",
            "shared/molecules/dashboard_card.html",
            "shared/organisms/entity_list_table.html",
            "forge/entity_list_table.html",
            "forge/entity_form.html",
            "forge/entity_detail.html",
            "cloud/base.html",
            "cloud/login.html",
            "cloud/dashboard.html",
            "cloud/entity_list.html",
        ];
        for name in &names {
            let result = engine.env.get_template(name);
            assert!(
                result.is_ok(),
                "engine failed to load template {name}: {:?}",
                result.err()
            );
        }
    }

    #[test]
    fn render_filesystem_template() {
        let engine = TemplateEngine::new(test_template_dir());
        #[derive(Serialize)]
        struct Card {
            url_name: String,
            label: String,
            display_value: String,
            widget_label: String,
        }
        #[derive(Serialize)]
        struct Ctx {
            card: Card,
        }
        let ctx = Ctx {
            card: Card {
                url_name: "Contact".into(),
                label: "Contacts".into(),
                display_value: "42".into(),
                widget_label: "Count".into(),
            },
        };
        let result = engine.render("shared/molecules/dashboard_card.html", &ctx);
        assert!(result.is_ok(), "render failed: {:?}", result.err());
        let html = result.unwrap();
        assert!(html.contains("Contacts"));
        assert!(html.contains("42"));
        assert!(html.contains("Count"));
    }

    #[test]
    fn split_filter_works() {
        let dir = tempfile::tempdir().unwrap();
        let tmpl_dir = dir.path().join("test");
        std::fs::create_dir_all(&tmpl_dir).unwrap();
        std::fs::write(
            tmpl_dir.join("split_test.html"),
            "{% for item in value | split(\", \") %}[{{ item }}]{% endfor %}",
        )
        .unwrap();

        let engine = TemplateEngine::new(dir.path().to_path_buf());
        #[derive(Serialize)]
        struct Ctx {
            value: String,
        }
        let result = engine
            .render(
                "test/split_test.html",
                &Ctx {
                    value: "a, b, c".into(),
                },
            )
            .unwrap();
        assert_eq!(result, "[a][b][c]");
    }

    #[test]
    fn missing_template_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let engine = TemplateEngine::new(dir.path().to_path_buf());
        let result = engine.render("nonexistent.html", &());
        assert!(result.is_err());
    }
}
