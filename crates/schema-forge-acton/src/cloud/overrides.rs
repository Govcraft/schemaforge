use std::path::PathBuf;

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

/// Look up an embedded template by path name.
///
/// Returns `None` if the name doesn't match any known template.
pub fn embedded_template(name: &str) -> Option<&'static str> {
    match name {
        "cloud/base.html" => Some(include_str!("../../templates/cloud/base.html")),
        "cloud/login.html" => Some(include_str!("../../templates/cloud/login.html")),
        "cloud/dashboard.html" => Some(include_str!("../../templates/cloud/dashboard.html")),
        "cloud/entity_list.html" => Some(include_str!("../../templates/cloud/entity_list.html")),
        "cloud/entity_list_kanban.html" => {
            Some(include_str!("../../templates/cloud/entity_list_kanban.html"))
        }
        "cloud/entity_form.html" => Some(include_str!("../../templates/cloud/entity_form.html")),
        "cloud/entity_detail.html" => {
            Some(include_str!("../../templates/cloud/entity_detail.html"))
        }
        "cloud/fragments/entity_list_body.html" => Some(include_str!(
            "../../templates/cloud/fragments/entity_list_body.html"
        )),
        "cloud/atoms/field_display.html" => {
            Some(include_str!("../../templates/cloud/atoms/field_display.html"))
        }
        "cloud/atoms/field_input.html" => {
            Some(include_str!("../../templates/cloud/atoms/field_input.html"))
        }
        "cloud/atoms/composite.html" => {
            Some(include_str!("../../templates/cloud/atoms/composite.html"))
        }
        "molecules/dashboard_card.html" => {
            Some(include_str!("../../templates/molecules/dashboard_card.html"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_embedded_template() {
        let engine = TemplateEngine::new(None);
        // dashboard_card.html is a simple template with {{ card.* }}
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
        let result = engine.render("molecules/dashboard_card.html", &ctx);
        assert!(result.is_ok(), "render failed: {:?}", result.err());
        let html = result.unwrap();
        assert!(html.contains("Contacts"));
        assert!(html.contains("42"));
        assert!(html.contains("Count"));
    }

    #[test]
    fn render_filesystem_override() {
        let dir = tempfile::tempdir().unwrap();
        let override_path = dir.path().join("molecules");
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
            .render("molecules/dashboard_card.html", &ctx)
            .unwrap();
        assert!(result.contains("OVERRIDE: Test"));
    }

    #[test]
    fn render_falls_back_to_embedded() {
        // An empty override dir — should still find embedded templates
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
            .render("molecules/dashboard_card.html", &ctx)
            .unwrap();
        assert!(result.contains("Fallback"));
    }

    #[test]
    fn render_with_extends() {
        // Verify that extends/block work: dashboard extends base.html
        let engine = TemplateEngine::new(None);
        #[derive(Serialize)]
        struct Ctx {
            app_title: String,
            nav_style: String,
            logo_url: Option<String>,
            nav_schemas: Vec<()>,
            active_nav: String,
            schema_cards: Vec<()>,
            current_user: Option<()>,
            favicon_url: Option<String>,
            head_html: Option<String>,
            nav_extra_html: Option<String>,
            footer_html: Option<String>,
        }
        let ctx = Ctx {
            app_title: "TestApp".into(),
            nav_style: "sidebar".into(),
            logo_url: None,
            nav_schemas: vec![],
            active_nav: "dashboard".into(),
            schema_cards: vec![],
            current_user: None,
            favicon_url: None,
            head_html: None,
            nav_extra_html: None,
            footer_html: None,
        };
        let result = engine.render("cloud/dashboard.html", &ctx);
        assert!(result.is_ok(), "extends render failed: {:?}", result.err());
        let html = result.unwrap();
        assert!(html.contains("TestApp"));
        assert!(html.contains("Dashboard"));
    }

    #[test]
    fn split_filter_works() {
        let _engine = TemplateEngine::new(None);

        // Create a temp override dir to test the split filter with a custom template
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
