use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::Environment;
use serde::Serialize;

/// MiniJinja-based template engine.
///
/// Widget/forge/shared templates are embedded in the binary via `include_str!()`.
/// Admin templates (when the admin-ui feature is enabled) are loaded from the
/// filesystem via an optional fallback loader.
pub struct TemplateEngine {
    env: Environment<'static>,
}

impl TemplateEngine {
    /// Create a new engine with embedded widget templates.
    ///
    /// When `template_dir` is `Some`, a filesystem fallback loader is added
    /// for templates not found in the embedded set (e.g. admin templates).
    /// When `site_template_dir` is `Some`, site templates are loaded from the
    /// filesystem first (allowing user customization), with embedded defaults
    /// as a fallback.
    /// When `None`, only embedded templates are available.
    pub fn new(
        template_dir: Option<std::path::PathBuf>,
        site_template_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let mut env = Environment::new();

        // --- Embedded templates (widget/forge/shared) ---

        // Forge entry-point templates
        env.add_template(
            "forge/entity_list.html",
            include_str!("../templates/forge/entity_list.html"),
        )
        .unwrap();
        env.add_template(
            "forge/entity_detail.html",
            include_str!("../templates/forge/entity_detail.html"),
        )
        .unwrap();
        env.add_template(
            "forge/entity_form.html",
            include_str!("../templates/forge/entity_form.html"),
        )
        .unwrap();

        // Shared organisms
        env.add_template(
            "shared/organisms/entity_list.html",
            include_str!("../templates/shared/organisms/entity_list.html"),
        )
        .unwrap();
        env.add_template(
            "shared/organisms/entity_detail.html",
            include_str!("../templates/shared/organisms/entity_detail.html"),
        )
        .unwrap();
        // Alias for handlers that reference "organisms/entity_detail.html" directly
        env.add_template(
            "organisms/entity_detail.html",
            include_str!("../templates/shared/organisms/entity_detail.html"),
        )
        .unwrap();

        // Shared molecules
        env.add_template(
            "shared/molecules/entity_row.html",
            include_str!("../templates/shared/molecules/entity_row.html"),
        )
        .unwrap();
        env.add_template(
            "shared/molecules/pagination.html",
            include_str!("../templates/shared/molecules/pagination.html"),
        )
        .unwrap();
        env.add_template(
            "shared/molecules/empty_state.html",
            include_str!("../templates/shared/molecules/empty_state.html"),
        )
        .unwrap();

        // Shared atoms — field display
        env.add_template(
            "shared/atoms/field_display.html",
            include_str!("../templates/shared/atoms/field_display.html"),
        )
        .unwrap();

        // Shared atoms — input types
        env.add_template(
            "shared/atoms/text_input.html",
            include_str!("../templates/shared/atoms/text_input.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/textarea.html",
            include_str!("../templates/shared/atoms/textarea.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/number_input.html",
            include_str!("../templates/shared/atoms/number_input.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/checkbox.html",
            include_str!("../templates/shared/atoms/checkbox.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/datetime_input.html",
            include_str!("../templates/shared/atoms/datetime_input.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/select.html",
            include_str!("../templates/shared/atoms/select.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/json_editor.html",
            include_str!("../templates/shared/atoms/json_editor.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/composite.html",
            include_str!("../templates/shared/atoms/composite.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/array_input.html",
            include_str!("../templates/shared/atoms/array_input.html"),
        )
        .unwrap();
        env.add_template(
            "shared/atoms/fallback_input.html",
            include_str!("../templates/shared/atoms/fallback_input.html"),
        )
        .unwrap();

        // Admin fragments (field input dispatcher used by forge and site forms)
        env.add_template(
            "admin/fragments/field_input.html",
            include_str!("../templates/admin/fragments/field_input.html"),
        )
        .unwrap();

        // --- Filesystem loader with embedded site template fallbacks ---
        //
        // Resolution order for a template name:
        //   1. Embedded templates registered via `add_template()` above (widget/forge/shared)
        //   2. Site template dir on filesystem (user-customizable, checked first for site/ names)
        //   3. Admin template dir on filesystem
        //   4. Embedded site template defaults (compiled into the binary)
        {
            let site_dir = site_template_dir;
            let admin_dir = template_dir;
            env.set_loader(move |name: &str| {
                // Check site template dir first (user customizations take priority)
                if let Some(ref dir) = site_dir {
                    // Site templates are scaffolded without the "site/" prefix,
                    // so strip it when looking up on the filesystem.
                    let fs_name = name.strip_prefix("site/").unwrap_or(name);
                    let path = dir.join(fs_name);
                    if path.is_file() {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            return Ok(Some(content));
                        }
                    }
                }

                // Check admin template dir
                if let Some(ref dir) = admin_dir {
                    let path = dir.join(name);
                    if path.is_file() {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            return Ok(Some(content));
                        }
                    }
                }

                // Embedded site template defaults
                match name {
                    "site/base.html" => Ok(Some(include_str!("../templates/site/base.html").to_string())),
                    "site/index.html" => Ok(Some(include_str!("../templates/site/index.html").to_string())),
                    "site/login.html" => Ok(Some(include_str!("../templates/site/login.html").to_string())),
                    "site/login_card.html" => Ok(Some(include_str!("../templates/site/login_card.html").to_string())),
                    "site/entities.html" => Ok(Some(include_str!("../templates/site/entities.html").to_string())),
                    "site/entity_detail.html" => Ok(Some(include_str!("../templates/site/entity_detail.html").to_string())),
                    "site/entity_form.html" => Ok(Some(include_str!("../templates/site/entity_form.html").to_string())),
                    _ => Ok(None),
                }
            });
        }

        // --- Custom filters ---

        // `split` filter: `field.value | split(sep=", ")`
        env.add_filter("split", |value: &str, sep: &str| -> Vec<String> {
            value.split(sep).map(|s| s.to_string()).collect()
        });

        // `truncate` filter: `value | truncate(length=N, end="...")`
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

    fn test_template_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
    }

    #[test]
    fn embedded_templates_loadable() {
        let engine = TemplateEngine::new(None, None);
        let names = [
            "forge/entity_list.html",
            "forge/entity_form.html",
            "forge/entity_detail.html",
            "shared/atoms/field_display.html",
            "shared/molecules/entity_row.html",
            "shared/molecules/pagination.html",
            "shared/organisms/entity_list.html",
            "shared/organisms/entity_detail.html",
            "organisms/entity_detail.html",
            "site/base.html",
            "site/index.html",
            "site/login.html",
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
    fn admin_filesystem_templates_loadable() {
        let engine = TemplateEngine::new(Some(test_template_dir()), None);
        let names = [
            "admin/base.html",
            "admin/login.html",
            "admin/dashboard.html",
            "admin/entity_list.html",
            "admin/entity_form.html",
            "admin/entity_detail.html",
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
    fn split_filter_works() {
        let dir = tempfile::tempdir().unwrap();
        let engine = TemplateEngine::new(Some(dir.path().to_path_buf()), None);
        // Use an embedded template that exercises the split filter
        // The field_display template uses `split` — test it directly
        #[derive(Serialize)]
        struct Ctx {
            value: String,
        }

        // Create a temp template to test the filter directly
        let tmpl_dir = dir.path().join("test");
        std::fs::create_dir_all(&tmpl_dir).unwrap();
        std::fs::write(
            tmpl_dir.join("split_test.html"),
            "{% for item in value | split(\", \") %}[{{ item }}]{% endfor %}",
        )
        .unwrap();

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
        let engine = TemplateEngine::new(None, None);
        let result = engine.render("nonexistent.html", &());
        assert!(result.is_err());
    }
}
