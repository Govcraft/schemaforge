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
        "shared/atoms/avatar.html" => {
            Some(include_str!("../templates/shared/atoms/avatar.html"))
        }
        "shared/atoms/heading_title.html" => {
            Some(include_str!("../templates/shared/atoms/heading_title.html"))
        }
        "shared/atoms/meta_item.html" => {
            Some(include_str!("../templates/shared/atoms/meta_item.html"))
        }
        "shared/atoms/stat_cell.html" => {
            Some(include_str!("../templates/shared/atoms/stat_cell.html"))
        }
        "shared/atoms/button.html" => {
            Some(include_str!("../templates/shared/atoms/button.html"))
        }
        "shared/atoms/stat_trend_badge.html" => {
            Some(include_str!("../templates/shared/atoms/stat_trend_badge.html"))
        }
        "shared/atoms/stat_trend_inline.html" => {
            Some(include_str!("../templates/shared/atoms/stat_trend_inline.html"))
        }
        "shared/atoms/stat_trend_text.html" => {
            Some(include_str!("../templates/shared/atoms/stat_trend_text.html"))
        }
        "shared/atoms/stat_icon_box.html" => {
            Some(include_str!("../templates/shared/atoms/stat_icon_box.html"))
        }
        "shared/atoms/initial_badge.html" => {
            Some(include_str!("../templates/shared/atoms/initial_badge.html"))
        }
        "shared/atoms/grid_action_arrow.html" => {
            Some(include_str!("../templates/shared/atoms/grid_action_arrow.html"))
        }
        "shared/atoms/action_divider.html" => {
            Some(include_str!("../templates/shared/atoms/action_divider.html"))
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
        "shared/molecules/button_group.html" => {
            Some(include_str!("../templates/shared/molecules/button_group.html"))
        }
        "shared/molecules/heading_breadcrumbs.html" => {
            Some(include_str!("../templates/shared/molecules/heading_breadcrumbs.html"))
        }
        "shared/molecules/meta_row.html" => {
            Some(include_str!("../templates/shared/molecules/meta_row.html"))
        }
        "shared/molecules/filter_tabs.html" => {
            Some(include_str!("../templates/shared/molecules/filter_tabs.html"))
        }
        "shared/molecules/stat_grid.html" => {
            Some(include_str!("../templates/shared/molecules/stat_grid.html"))
        }
        "shared/molecules/mobile_dropdown.html" => {
            Some(include_str!("../templates/shared/molecules/mobile_dropdown.html"))
        }
        "shared/molecules/stat_item_simple.html" => {
            Some(include_str!("../templates/shared/molecules/stat_item_simple.html"))
        }
        "shared/molecules/stat_item_card.html" => {
            Some(include_str!("../templates/shared/molecules/stat_item_card.html"))
        }
        "shared/molecules/stat_item_icon.html" => {
            Some(include_str!("../templates/shared/molecules/stat_item_icon.html"))
        }
        "shared/molecules/stat_item_comparison.html" => {
            Some(include_str!("../templates/shared/molecules/stat_item_comparison.html"))
        }
        "shared/molecules/stat_item_trending.html" => {
            Some(include_str!("../templates/shared/molecules/stat_item_trending.html"))
        }
        "shared/molecules/grid_item_badge.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_badge.html"))
        }
        "shared/molecules/grid_item_profile.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_profile.html"))
        }
        "shared/molecules/grid_item_directory.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_directory.html"))
        }
        "shared/molecules/grid_item_link.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_link.html"))
        }
        "shared/molecules/grid_item_gallery.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_gallery.html"))
        }
        "shared/molecules/grid_item_detail.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_detail.html"))
        }
        "shared/molecules/grid_item_action.html" => {
            Some(include_str!("../templates/shared/molecules/grid_item_action.html"))
        }
        "shared/molecules/grid_card_actions.html" => {
            Some(include_str!("../templates/shared/molecules/grid_card_actions.html"))
        }
        "shared/molecules/grid_card_inline_actions.html" => {
            Some(include_str!("../templates/shared/molecules/grid_card_inline_actions.html"))
        }
        "shared/molecules/grid_entity_dl.html" => {
            Some(include_str!("../templates/shared/molecules/grid_entity_dl.html"))
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
        "shared/organisms/heading_with_actions.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_actions.html"))
        }
        "shared/organisms/heading_with_actions_and_breadcrumbs.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_actions_and_breadcrumbs.html"))
        }
        "shared/organisms/heading_card_with_avatar_and_stats.html" => {
            Some(include_str!("../templates/shared/organisms/heading_card_with_avatar_and_stats.html"))
        }
        "shared/organisms/heading_with_avatar_and_actions.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_avatar_and_actions.html"))
        }
        "shared/organisms/heading_with_banner_image.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_banner_image.html"))
        }
        "shared/organisms/heading_with_filters_and_actions.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_filters_and_actions.html"))
        }
        "shared/organisms/heading_with_logo_meta_and_actions.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_logo_meta_and_actions.html"))
        }
        "shared/organisms/heading_with_meta_actions_and_breadcrumbs.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_meta_actions_and_breadcrumbs.html"))
        }
        "shared/organisms/heading_with_meta_and_actions.html" => {
            Some(include_str!("../templates/shared/organisms/heading_with_meta_and_actions.html"))
        }
        "shared/organisms/stats_simple.html" => {
            Some(include_str!("../templates/shared/organisms/stats_simple.html"))
        }
        "shared/organisms/stats_cards.html" => {
            Some(include_str!("../templates/shared/organisms/stats_cards.html"))
        }
        "shared/organisms/stats_with_icons.html" => {
            Some(include_str!("../templates/shared/organisms/stats_with_icons.html"))
        }
        "shared/organisms/stats_shared_borders.html" => {
            Some(include_str!("../templates/shared/organisms/stats_shared_borders.html"))
        }
        "shared/organisms/stats_trending.html" => {
            Some(include_str!("../templates/shared/organisms/stats_trending.html"))
        }
        "shared/organisms/stats_grid_actions.html" => {
            Some(include_str!(
                "../templates/shared/organisms/stats_grid_actions.html"
            ))
        }
        "shared/organisms/stats_grid_badge.html" => {
            Some(include_str!(
                "../templates/shared/organisms/stats_grid_badge.html"
            ))
        }
        "shared/organisms/stats_section.html" => {
            Some(include_str!("../templates/shared/organisms/stats_section.html"))
        }
        "shared/organisms/card_basic.html" => {
            Some(include_str!("../templates/shared/organisms/card_basic.html"))
        }
        "shared/organisms/card_edge_to_edge.html" => {
            Some(include_str!("../templates/shared/organisms/card_edge_to_edge.html"))
        }
        "shared/organisms/card_with_header.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_header.html"))
        }
        "shared/organisms/card_with_footer.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_footer.html"))
        }
        "shared/organisms/card_with_header_and_footer.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_header_and_footer.html"))
        }
        "shared/organisms/card_with_gray_body.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_gray_body.html"))
        }
        "shared/organisms/card_with_gray_footer.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_gray_footer.html"))
        }
        "shared/organisms/card_with_well.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_well.html"))
        }
        "shared/organisms/card_with_well_edge_to_edge.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_well_edge_to_edge.html"))
        }
        "shared/organisms/card_with_well_on_gray.html" => {
            Some(include_str!("../templates/shared/organisms/card_with_well_on_gray.html"))
        }
        "shared/organisms/container_standard.html" => {
            Some(include_str!("../templates/shared/organisms/container_standard.html"))
        }
        "shared/organisms/container_full_mobile.html" => {
            Some(include_str!("../templates/shared/organisms/container_full_mobile.html"))
        }
        "shared/organisms/container_breakpoint.html" => {
            Some(include_str!("../templates/shared/organisms/container_breakpoint.html"))
        }
        "shared/organisms/container_breakpoint_full_mobile.html" => {
            Some(include_str!("../templates/shared/organisms/container_breakpoint_full_mobile.html"))
        }
        "shared/organisms/container_narrow.html" => {
            Some(include_str!("../templates/shared/organisms/container_narrow.html"))
        }
        "shared/organisms/entity_list_grid_badge.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_badge.html"))
        }
        "shared/organisms/entity_list_grid_profile.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_profile.html"))
        }
        "shared/organisms/entity_list_grid_directory.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_directory.html"))
        }
        "shared/organisms/entity_list_grid_link.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_link.html"))
        }
        "shared/organisms/entity_list_grid_gallery.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_gallery.html"))
        }
        "shared/organisms/entity_list_grid_detail.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_detail.html"))
        }
        "shared/organisms/entity_list_grid_actions.html" => {
            Some(include_str!("../templates/shared/organisms/entity_list_grid_actions.html"))
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
        "cloud/atoms/sidebar_macros.html" => {
            Some(include_str!("../templates/cloud/atoms/sidebar_macros.html"))
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
        "cloud/shells/stacked_page_header.html" => {
            Some(include_str!("../templates/cloud/shells/stacked_page_header.html"))
        }
        "cloud/shells/stacked_tab.html" => {
            Some(include_str!("../templates/cloud/shells/stacked_tab.html"))
        }
        "cloud/shells/sidebar.html" => {
            Some(include_str!("../templates/cloud/shells/sidebar.html"))
        }
        "cloud/shells/sidebar_simple.html" => {
            Some(include_str!("../templates/cloud/shells/sidebar_simple.html"))
        }
        "cloud/shells/multicolumn_constrained.html" => {
            Some(include_str!("../templates/cloud/shells/multicolumn_constrained.html"))
        }
        "cloud/shells/multicolumn_sidebar.html" => {
            Some(include_str!("../templates/cloud/shells/multicolumn_sidebar.html"))
        }
        "cloud/shells/multicolumn_narrow.html" => {
            Some(include_str!("../templates/cloud/shells/multicolumn_narrow.html"))
        }

        // -----------------------------------------------------------------
        // Cloud shell molecules (shared building blocks for shells)
        // -----------------------------------------------------------------
        "cloud/molecules/shell_logo.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_logo.html"))
        }
        "cloud/molecules/shell_sidebar_nav.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_sidebar_nav.html"))
        }
        "cloud/molecules/shell_stacked_nav_inner.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_stacked_nav_inner.html"))
        }
        "cloud/molecules/shell_mobile_disclosure.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_mobile_disclosure.html"))
        }
        "cloud/molecules/shell_header_user_controls.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_header_user_controls.html"))
        }
        "cloud/molecules/shell_sidebar_mobile_bar.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_sidebar_mobile_bar.html"))
        }
        "cloud/molecules/shell_sidebar_icon_nav.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_sidebar_icon_nav.html"))
        }
        "cloud/molecules/shell_stacked_tab_nav_inner.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_stacked_tab_nav_inner.html"))
        }
        "cloud/molecules/shell_stacked_page_header.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_stacked_page_header.html"))
        }
        "cloud/molecules/shell_multicolumn_header.html" => {
            Some(include_str!("../templates/cloud/molecules/shell_multicolumn_header.html"))
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
            "shared/atoms/avatar.html",
            "shared/atoms/heading_title.html",
            "shared/atoms/meta_item.html",
            "shared/atoms/stat_cell.html",
            "shared/atoms/button.html",
            "shared/atoms/stat_trend_badge.html",
            "shared/atoms/stat_trend_inline.html",
            "shared/atoms/stat_trend_text.html",
            "shared/atoms/stat_icon_box.html",
            "shared/atoms/initial_badge.html",
            "shared/atoms/grid_action_arrow.html",
            "shared/atoms/action_divider.html",
            "shared/molecules/dashboard_card.html",
            "shared/molecules/entity_row.html",
            "shared/molecules/pagination.html",
            "shared/molecules/breadcrumbs.html",
            "shared/molecules/page_header.html",
            "shared/molecules/alert.html",
            "shared/molecules/empty_state.html",
            "shared/molecules/button_group.html",
            "shared/molecules/heading_breadcrumbs.html",
            "shared/molecules/meta_row.html",
            "shared/molecules/filter_tabs.html",
            "shared/molecules/stat_grid.html",
            "shared/molecules/mobile_dropdown.html",
            "shared/molecules/stat_item_simple.html",
            "shared/molecules/stat_item_card.html",
            "shared/molecules/stat_item_icon.html",
            "shared/molecules/stat_item_comparison.html",
            "shared/molecules/stat_item_trending.html",
            "shared/molecules/grid_item_badge.html",
            "shared/molecules/grid_item_profile.html",
            "shared/molecules/grid_item_directory.html",
            "shared/molecules/grid_item_link.html",
            "shared/molecules/grid_item_gallery.html",
            "shared/molecules/grid_item_detail.html",
            "shared/molecules/grid_item_action.html",
            "shared/molecules/grid_card_actions.html",
            "shared/molecules/grid_card_inline_actions.html",
            "shared/molecules/grid_entity_dl.html",
            "shared/organisms/entity_list_table.html",
            "shared/organisms/entity_list_cards.html",
            "shared/organisms/entity_list_compact.html",
            "shared/organisms/entity_detail_full.html",
            "shared/organisms/entity_detail_split.html",
            "shared/organisms/entity_detail_tabbed.html",
            "shared/organisms/heading_with_actions.html",
            "shared/organisms/heading_with_actions_and_breadcrumbs.html",
            "shared/organisms/heading_card_with_avatar_and_stats.html",
            "shared/organisms/heading_with_avatar_and_actions.html",
            "shared/organisms/heading_with_banner_image.html",
            "shared/organisms/heading_with_filters_and_actions.html",
            "shared/organisms/heading_with_logo_meta_and_actions.html",
            "shared/organisms/heading_with_meta_actions_and_breadcrumbs.html",
            "shared/organisms/heading_with_meta_and_actions.html",
            "shared/organisms/stats_simple.html",
            "shared/organisms/stats_cards.html",
            "shared/organisms/stats_with_icons.html",
            "shared/organisms/stats_shared_borders.html",
            "shared/organisms/stats_trending.html",
            "shared/organisms/stats_grid_actions.html",
            "shared/organisms/stats_grid_badge.html",
            "shared/organisms/stats_section.html",
            "shared/organisms/card_basic.html",
            "shared/organisms/card_edge_to_edge.html",
            "shared/organisms/card_with_header.html",
            "shared/organisms/card_with_footer.html",
            "shared/organisms/card_with_header_and_footer.html",
            "shared/organisms/card_with_gray_body.html",
            "shared/organisms/card_with_gray_footer.html",
            "shared/organisms/card_with_well.html",
            "shared/organisms/card_with_well_edge_to_edge.html",
            "shared/organisms/card_with_well_on_gray.html",
            "shared/organisms/container_standard.html",
            "shared/organisms/container_full_mobile.html",
            "shared/organisms/container_breakpoint.html",
            "shared/organisms/container_breakpoint_full_mobile.html",
            "shared/organisms/container_narrow.html",
            "shared/organisms/entity_list_grid_badge.html",
            "shared/organisms/entity_list_grid_profile.html",
            "shared/organisms/entity_list_grid_directory.html",
            "shared/organisms/entity_list_grid_link.html",
            "shared/organisms/entity_list_grid_gallery.html",
            "shared/organisms/entity_list_grid_detail.html",
            "shared/organisms/entity_list_grid_actions.html",
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
            "cloud/atoms/sidebar_macros.html",
            "cloud/base.css",
            "cloud/shells/stacked.html",
            "cloud/shells/stacked_overlap.html",
            "cloud/shells/stacked_page_header.html",
            "cloud/shells/stacked_tab.html",
            "cloud/shells/sidebar.html",
            "cloud/shells/sidebar_simple.html",
            "cloud/shells/multicolumn_constrained.html",
            "cloud/shells/multicolumn_sidebar.html",
            "cloud/shells/multicolumn_narrow.html",
            "cloud/molecules/shell_logo.html",
            "cloud/molecules/shell_sidebar_nav.html",
            "cloud/molecules/shell_stacked_nav_inner.html",
            "cloud/molecules/shell_mobile_disclosure.html",
            "cloud/molecules/shell_header_user_controls.html",
            "cloud/molecules/shell_sidebar_mobile_bar.html",
            "cloud/molecules/shell_sidebar_icon_nav.html",
            "cloud/molecules/shell_stacked_tab_nav_inner.html",
            "cloud/molecules/shell_stacked_page_header.html",
            "cloud/molecules/shell_multicolumn_header.html",
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
