use std::path::PathBuf;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::Environment;
use serde::Serialize;

/// MiniJinja-based template engine with embedded defaults and filesystem override support.
///
/// Templates are loaded from two sources in priority order:
/// 1. Filesystem override directory (user-provided customizations)
/// 2. Embedded defaults compiled into the binary via `include_str!()`
pub struct TemplateEngine {
    env: Environment<'static>,
}

impl TemplateEngine {
    /// Create a new engine with optional filesystem override directory.
    /// Embedded defaults always available as fallback.
    pub fn new(override_dir: Option<PathBuf>) -> Self {
        let mut env = Environment::new();

        // Set up a source that checks filesystem first, then embedded
        env.set_loader(move |name| {
            // 1. Check filesystem override
            if let Some(ref dir) = override_dir {
                let path = dir.join(name);
                if path.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        return Ok(Some(content));
                    }
                }
            }

            // 2. Fall back to embedded template
            Ok(embedded_template(name).map(|s| s.to_string()))
        });

        // Register custom `split` filter for `field.value | split(", ")`
        env.add_filter("split", |value: &str, sep: &str| -> Vec<String> {
            value.split(sep).map(|s| s.to_string()).collect()
        });

        Self { env }
    }

    /// Render a template by name with a serializable context.
    pub fn render<T: Serialize>(&self, name: &str, ctx: &T) -> Result<String, String> {
        let tmpl = self.env.get_template(name).map_err(|e| e.to_string())?;
        tmpl.render(ctx).map_err(|e| e.to_string())
    }
}

/// Render a full-page template (Content-Type: text/html).
pub fn render_template<T: Serialize>(
    engine: &TemplateEngine,
    name: &str,
    ctx: &T,
) -> Response {
    match engine.render(name, ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {e}")).into_response()
        }
    }
}

/// Render a template fragment (Content-Type: text/html, no layout wrapping).
/// Same as render_template but semantically for HTMX fragments.
pub fn render_fragment<T: Serialize>(
    engine: &TemplateEngine,
    name: &str,
    ctx: &T,
) -> Response {
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
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {e}")).into_response()
        }
    }
}

/// Look up an embedded template by path name.
///
/// Returns `None` if the name doesn't match any known template.
pub fn embedded_template(name: &str) -> Option<&'static str> {
    match name {
        // -----------------------------------------------------------------
        // Admin pages
        // -----------------------------------------------------------------
        "admin/base.html" => Some(include_str!("../templates/admin/base.html")),
        "admin/login.html" => Some(include_str!("../templates/admin/login.html")),
        "admin/dashboard.html" => Some(include_str!("../templates/admin/dashboard.html")),
        "admin/schema_detail.html" => Some(include_str!("../templates/admin/schema_detail.html")),
        "admin/schema_editor.html" => Some(include_str!("../templates/admin/schema_editor.html")),
        "admin/entity_list.html" => Some(include_str!("../templates/admin/entity_list.html")),
        "admin/entity_form.html" => Some(include_str!("../templates/admin/entity_form.html")),
        "admin/entity_detail.html" => Some(include_str!("../templates/admin/entity_detail.html")),
        "admin/user_list.html" => Some(include_str!("../templates/admin/user_list.html")),
        "admin/user_form.html" => Some(include_str!("../templates/admin/user_form.html")),

        // -----------------------------------------------------------------
        // Admin fragments
        // -----------------------------------------------------------------
        "admin/fragments/entity_table_body.html" => {
            Some(include_str!("../templates/admin/fragments/entity_table_body.html"))
        }
        "admin/fragments/flash_message.html" => {
            Some(include_str!("../templates/admin/fragments/flash_message.html"))
        }
        "admin/fragments/relation_options.html" => {
            Some(include_str!("../templates/admin/fragments/relation_options.html"))
        }
        "admin/fragments/field_editor_row.html" => {
            Some(include_str!("../templates/admin/fragments/field_editor_row.html"))
        }
        "admin/fragments/type_constraints.html" => {
            Some(include_str!("../templates/admin/fragments/type_constraints.html"))
        }
        "admin/fragments/dsl_preview.html" => {
            Some(include_str!("../templates/admin/fragments/dsl_preview.html"))
        }
        "admin/fragments/migration_preview.html" => {
            Some(include_str!("../templates/admin/fragments/migration_preview.html"))
        }
        "admin/fragments/field_input.html" => {
            Some(include_str!("../templates/admin/fragments/field_input.html"))
        }

        // -----------------------------------------------------------------
        // Shared atoms (used by admin + widget templates via include)
        // -----------------------------------------------------------------
        "shared/atoms/field_display.html" => {
            Some(include_str!("../templates/shared/atoms/field_display.html"))
        }
        "shared/atoms/text_input.html" => {
            Some(include_str!("../templates/shared/atoms/text_input.html"))
        }
        "shared/atoms/textarea.html" => {
            Some(include_str!("../templates/shared/atoms/textarea.html"))
        }
        "shared/atoms/number_input.html" => {
            Some(include_str!("../templates/shared/atoms/number_input.html"))
        }
        "shared/atoms/checkbox.html" => {
            Some(include_str!("../templates/shared/atoms/checkbox.html"))
        }
        "shared/atoms/datetime_input.html" => {
            Some(include_str!("../templates/shared/atoms/datetime_input.html"))
        }
        "shared/atoms/select.html" => {
            Some(include_str!("../templates/shared/atoms/select.html"))
        }
        "shared/atoms/json_editor.html" => {
            Some(include_str!("../templates/shared/atoms/json_editor.html"))
        }
        "shared/atoms/array_input.html" => {
            Some(include_str!("../templates/shared/atoms/array_input.html"))
        }
        "shared/atoms/composite.html" => {
            Some(include_str!("../templates/shared/atoms/composite.html"))
        }
        "shared/atoms/fallback_input.html" => {
            Some(include_str!("../templates/shared/atoms/fallback_input.html"))
        }

        // -----------------------------------------------------------------
        // Shared molecules
        // -----------------------------------------------------------------
        "shared/molecules/dashboard_card.html" => {
            Some(include_str!("../templates/shared/molecules/dashboard_card.html"))
        }
        "shared/molecules/entity_row.html" => {
            Some(include_str!("../templates/shared/molecules/entity_row.html"))
        }
        "shared/molecules/pagination.html" => {
            Some(include_str!("../templates/shared/molecules/pagination.html"))
        }
        "shared/molecules/breadcrumbs.html" => {
            Some(include_str!("../templates/shared/molecules/breadcrumbs.html"))
        }
        "shared/molecules/page_header.html" => {
            Some(include_str!("../templates/shared/molecules/page_header.html"))
        }
        "shared/molecules/alert.html" => {
            Some(include_str!("../templates/shared/molecules/alert.html"))
        }
        "shared/molecules/empty_state.html" => {
            Some(include_str!("../templates/shared/molecules/empty_state.html"))
        }

        // -----------------------------------------------------------------
        // Shared organisms
        // -----------------------------------------------------------------
        "shared/organisms/entity_list_table.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_table.html"))
        }
        "shared/organisms/entity_list_cards.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_cards.html"))
        }
        "shared/organisms/entity_list_compact.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_compact.html"))
        }
        "shared/organisms/entity_detail_full.html" => {
            Some(include_str!("../templates/shared/organisms/entity_detail_full.html"))
        }
        "shared/organisms/entity_detail_split.html" => {
            Some(include_str!("../templates/shared/organisms/entity_detail_split.html"))
        }
        "shared/organisms/entity_detail_tabbed.html" => {
            Some(include_str!("../templates/shared/organisms/entity_detail_tabbed.html"))
        }

        // -----------------------------------------------------------------
        // Forge (widget) bare fragments
        // -----------------------------------------------------------------
        "forge/entity_list_table.html" => {
            Some(include_str!("../templates/forge/entity_list_table.html"))
        }
        "forge/entity_list_cards.html" => {
            Some(include_str!("../templates/forge/entity_list_cards.html"))
        }
        "forge/entity_list_compact.html" => {
            Some(include_str!("../templates/forge/entity_list_compact.html"))
        }
        "forge/entity_table.html" => {
            Some(include_str!("../templates/forge/entity_table.html"))
        }
        "forge/entity_detail.html" => {
            Some(include_str!("../templates/forge/entity_detail.html"))
        }
        "forge/entity_form.html" => {
            Some(include_str!("../templates/forge/entity_form.html"))
        }

        // -----------------------------------------------------------------
        // Cloud pages + fragments + atoms
        // -----------------------------------------------------------------
        "cloud/base.html" => Some(include_str!("../templates/cloud/base.html")),
        "cloud/login.html" => Some(include_str!("../templates/cloud/login.html")),
        "cloud/dashboard.html" => Some(include_str!("../templates/cloud/dashboard.html")),
        "cloud/entity_list.html" => Some(include_str!("../templates/cloud/entity_list.html")),
        "cloud/entity_list_kanban.html" => {
            Some(include_str!("../templates/cloud/entity_list_kanban.html"))
        }
        "cloud/entity_form.html" => Some(include_str!("../templates/cloud/entity_form.html")),
        "cloud/entity_detail.html" => {
            Some(include_str!("../templates/cloud/entity_detail.html"))
        }
        "cloud/fragments/entity_list_body.html" => Some(include_str!(
            "../templates/cloud/fragments/entity_list_body.html"
        )),
        "cloud/atoms/field_display.html" => {
            Some(include_str!("../templates/cloud/atoms/field_display.html"))
        }
        "cloud/atoms/field_input.html" => {
            Some(include_str!("../templates/cloud/atoms/field_input.html"))
        }
        "cloud/atoms/composite.html" => {
            Some(include_str!("../templates/cloud/atoms/composite.html"))
        }
        "cloud/base.css" => Some(include_str!("../templates/cloud/base.css")),

        // -----------------------------------------------------------------
        // Cloud shell variants (included by cloud/base.html)
        // -----------------------------------------------------------------
        "cloud/shells/stacked.html" => {
            Some(include_str!("../templates/cloud/shells/stacked.html"))
        }
        "cloud/shells/stacked_overlap.html" => {
            Some(include_str!("../templates/cloud/shells/stacked_overlap.html"))
        }
        "cloud/shells/stacked_compact.html" => {
            Some(include_str!("../templates/cloud/shells/stacked_compact.html"))
        }
        "cloud/shells/sidebar.html" => {
            Some(include_str!("../templates/cloud/shells/sidebar.html"))
        }

        // -----------------------------------------------------------------
        // Backward-compatible aliases (old paths -> shared)
        // -----------------------------------------------------------------
        "atoms/field_display.html" => embedded_template("shared/atoms/field_display.html"),
        "atoms/text_input.html" => embedded_template("shared/atoms/text_input.html"),
        "atoms/textarea.html" => embedded_template("shared/atoms/textarea.html"),
        "atoms/number_input.html" => embedded_template("shared/atoms/number_input.html"),
        "atoms/checkbox.html" => embedded_template("shared/atoms/checkbox.html"),
        "atoms/datetime_input.html" => embedded_template("shared/atoms/datetime_input.html"),
        "atoms/select.html" => embedded_template("shared/atoms/select.html"),
        "atoms/json_editor.html" => embedded_template("shared/atoms/json_editor.html"),
        "atoms/array_input.html" => embedded_template("shared/atoms/array_input.html"),
        "atoms/composite.html" => embedded_template("shared/atoms/composite.html"),
        "atoms/fallback_input.html" => embedded_template("shared/atoms/fallback_input.html"),
        "molecules/dashboard_card.html" => embedded_template("shared/molecules/dashboard_card.html"),
        "molecules/entity_row.html" => embedded_template("shared/molecules/entity_row.html"),
        "molecules/pagination.html" => embedded_template("shared/molecules/pagination.html"),
        "organisms/entity_list_table.html" => embedded_template("shared/organisms/entity_list_table.html"),
        "organisms/entity_list_cards.html" => embedded_template("shared/organisms/entity_list_cards.html"),
        "organisms/entity_list_compact.html" => embedded_template("shared/organisms/entity_list_compact.html"),
        "organisms/entity_detail_full.html" => embedded_template("shared/organisms/entity_detail_full.html"),
        "organisms/entity_detail_split.html" => embedded_template("shared/organisms/entity_detail_split.html"),
        "organisms/entity_detail_tabbed.html" => embedded_template("shared/organisms/entity_detail_tabbed.html"),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_templates_loadable() {
        let engine = TemplateEngine::new(None);
        // Smoke test: all registered templates can be fetched
        let names = [
            "admin/base.html",
            "admin/login.html",
            "admin/dashboard.html",
            "admin/entity_list.html",
            "admin/entity_form.html",
            "admin/entity_detail.html",
            "admin/schema_detail.html",
            "admin/schema_editor.html",
            "admin/user_list.html",
            "admin/user_form.html",
            "admin/fragments/entity_table_body.html",
            "admin/fragments/flash_message.html",
            "admin/fragments/relation_options.html",
            "admin/fragments/field_editor_row.html",
            "admin/fragments/type_constraints.html",
            "admin/fragments/dsl_preview.html",
            "admin/fragments/migration_preview.html",
            "admin/fragments/field_input.html",
            "shared/atoms/field_display.html",
            "shared/atoms/text_input.html",
            "shared/atoms/textarea.html",
            "shared/atoms/number_input.html",
            "shared/atoms/checkbox.html",
            "shared/atoms/datetime_input.html",
            "shared/atoms/select.html",
            "shared/atoms/json_editor.html",
            "shared/atoms/array_input.html",
            "shared/atoms/composite.html",
            "shared/atoms/fallback_input.html",
            "shared/molecules/dashboard_card.html",
            "shared/molecules/entity_row.html",
            "shared/molecules/pagination.html",
            "shared/molecules/breadcrumbs.html",
            "shared/molecules/page_header.html",
            "shared/molecules/alert.html",
            "shared/molecules/empty_state.html",
            "shared/organisms/entity_list_table.html",
            "shared/organisms/entity_list_cards.html",
            "shared/organisms/entity_list_compact.html",
            "shared/organisms/entity_detail_full.html",
            "shared/organisms/entity_detail_split.html",
            "shared/organisms/entity_detail_tabbed.html",
            "forge/entity_list_table.html",
            "forge/entity_list_cards.html",
            "forge/entity_list_compact.html",
            "forge/entity_table.html",
            "forge/entity_detail.html",
            "forge/entity_form.html",
            "cloud/base.html",
            "cloud/login.html",
            "cloud/dashboard.html",
            "cloud/entity_list.html",
            "cloud/entity_list_kanban.html",
            "cloud/entity_form.html",
            "cloud/entity_detail.html",
            "cloud/fragments/entity_list_body.html",
            "cloud/atoms/field_display.html",
            "cloud/atoms/field_input.html",
            "cloud/atoms/composite.html",
            "cloud/base.css",
            "cloud/shells/stacked.html",
            "cloud/shells/stacked_overlap.html",
            "cloud/shells/stacked_compact.html",
            "cloud/shells/sidebar.html",
        ];
        for name in &names {
            assert!(
                embedded_template(name).is_some(),
                "embedded_template missing: {name}"
            );
        }
        // Verify engine can load them
        for name in &names {
            let result = engine.env.get_template(name);
            assert!(result.is_ok(), "engine failed to load template {name}: {:?}", result.err());
        }
    }

    #[test]
    fn render_embedded_template() {
        let engine = TemplateEngine::new(None);
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
    fn render_filesystem_override() {
        let dir = tempfile::tempdir().unwrap();
        let override_path = dir.path().join("shared").join("molecules");
        std::fs::create_dir_all(&override_path).unwrap();
        std::fs::write(
            override_path.join("dashboard_card.html"),
            "<div>OVERRIDE: {{ card.label }}</div>",
        )
        .unwrap();

        let engine = TemplateEngine::new(Some(dir.path().to_path_buf()));

        #[derive(Serialize)]
        struct Card {
            label: String,
        }
        #[derive(Serialize)]
        struct Ctx {
            card: Card,
        }
        let ctx = Ctx {
            card: Card {
                label: "Test".into(),
            },
        };
        let result = engine
            .render("shared/molecules/dashboard_card.html", &ctx)
            .unwrap();
        assert!(result.contains("OVERRIDE: Test"));
    }

    #[test]
    fn render_falls_back_to_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let engine = TemplateEngine::new(Some(dir.path().to_path_buf()));

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
                url_name: "X".into(),
                label: "Fallback".into(),
                display_value: "99".into(),
                widget_label: "Count".into(),
            },
        };
        let result = engine
            .render("shared/molecules/dashboard_card.html", &ctx)
            .unwrap();
        assert!(result.contains("Fallback"));
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

        let engine = TemplateEngine::new(Some(dir.path().to_path_buf()));
        #[derive(Serialize)]
        struct Ctx {
            value: String,
        }
        let result = engine
            .render("test/split_test.html", &Ctx { value: "a, b, c".into() })
            .unwrap();
        assert_eq!(result, "[a][b][c]");
    }
}
