use crate::theme::Theme;

/// Generate the complete CSS for the cloud UI from a theme.
pub fn generate_css(theme: &Theme) -> String {
    let mut css = theme.to_css_vars();
    css.push_str(BASE_SF_STYLES);
    if let Some(custom) = &theme.custom_css {
        css.push_str(&sanitize_css(custom));
    }
    css
}

/// Sanitize user-provided CSS to prevent injection attacks.
///
/// Strips:
/// - `@import` rules (prevent loading external stylesheets)
/// - `url()` values (prevent loading external resources)
/// - `javascript:` protocol (XSS)
/// - `expression()` (IE CSS expressions)
/// - `behavior:` property (IE HTC)
fn sanitize_css(css: &str) -> String {
    let mut result = String::with_capacity(css.len());
    for line in css.lines() {
        let trimmed = line.trim().to_lowercase();
        // Skip entire line if it starts with @import
        if trimmed.starts_with("@import") {
            continue;
        }
        // Skip lines containing dangerous constructs
        if trimmed.contains("url(")
            || trimmed.contains("javascript:")
            || trimmed.contains("expression(")
            || trimmed.contains("behavior:")
        {
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

/// Base semantic styles for sf-* classes, driven by CSS custom properties.
const BASE_SF_STYLES: &str = r#"
/* SchemaForge Cloud UI — Base Styles */

/* Reset */
.sf-app { font-family: var(--sf-font-family); color: var(--sf-text); background: var(--sf-background); }
.sf-app *, .sf-app *::before, .sf-app *::after { box-sizing: border-box; }

/* Layout — Navigation */
.sf-nav { background: var(--sf-surface); border-color: var(--sf-secondary); }
.sf-nav[data-nav-style="sidebar"] { display: flex; flex-direction: column; width: 16rem; min-height: 100vh; border-right: 1px solid var(--sf-secondary); padding: var(--sf-density-padding); }
.sf-nav[data-nav-style="topnav"] { display: flex; flex-direction: row; align-items: center; gap: 1rem; border-bottom: 1px solid var(--sf-secondary); padding: var(--sf-density-padding) 1rem; }
.sf-nav[data-nav-style="minimal"] { display: flex; flex-direction: row; align-items: center; gap: 1rem; padding: var(--sf-density-padding) 1rem; }

.sf-nav-brand { font-weight: 700; font-size: 1.125rem; color: var(--sf-primary); }
.sf-nav-link { display: block; padding: 0.375rem 0.75rem; border-radius: var(--sf-border-radius); color: var(--sf-text); text-decoration: none; }
.sf-nav-link:hover { background: var(--sf-primary); color: #fff; }
.sf-nav-link[aria-current="page"] { background: var(--sf-primary); color: #fff; }

/* Layout — Main content */
.sf-layout { display: flex; min-height: 100vh; }
.sf-layout[data-nav-style="topnav"], .sf-layout[data-nav-style="minimal"] { flex-direction: column; }
.sf-main { flex: 1; padding: 1.5rem; }

/* Dashboard */
.sf-dashboard { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 1rem; }
.sf-dashboard-card { background: var(--sf-surface); border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); padding: 1.25rem; text-decoration: none; color: inherit; transition: box-shadow 0.15s; }
.sf-dashboard-card:hover { box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1); }
.sf-dashboard-card-title { font-weight: 600; font-size: 1rem; margin-bottom: 0.25rem; }
.sf-dashboard-card-value { font-size: 2rem; font-weight: 700; color: var(--sf-primary); }
.sf-dashboard-card-widget-label { font-size: 0.75rem; color: var(--sf-secondary); text-transform: uppercase; letter-spacing: 0.05em; }

/* Entity list — table variant */
.sf-entity-list { width: 100%; }
.sf-entity-list[data-list-style="table"] { border-collapse: collapse; display: table; }
.sf-entity-list[data-list-style="table"] .sf-list-header { display: table-header-group; }
.sf-entity-list[data-list-style="table"] .sf-list-header-cell { display: table-cell; padding: var(--sf-density-padding); font-weight: 600; text-align: left; border-bottom: 2px solid var(--sf-secondary); }
.sf-entity-list[data-list-style="table"] .sf-list-row { display: table-row; }
.sf-entity-list[data-list-style="table"] .sf-list-cell { display: table-cell; padding: var(--sf-density-padding); border-bottom: 1px solid var(--sf-surface); }

/* Entity list — cards variant */
.sf-entity-list[data-list-style="cards"] { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: var(--sf-density-padding); }

/* Entity list — compact variant */
.sf-entity-list[data-list-style="compact"] { display: flex; flex-direction: column; }
.sf-entity-list[data-list-style="compact"] .sf-list-row { display: flex; align-items: center; gap: 0.5rem; padding: 0.25rem var(--sf-density-padding); border-bottom: 1px solid var(--sf-surface); }

/* Entity card (used in cards + dashboard) */
.sf-card { background: var(--sf-surface); border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); padding: 1rem; }
.sf-card-title { font-weight: 600; margin-bottom: 0.5rem; }
.sf-card-field { display: flex; gap: 0.5rem; font-size: 0.875rem; margin-bottom: 0.25rem; }
.sf-card-label { color: var(--sf-secondary); min-width: 6rem; }

/* Entity detail */
.sf-detail { background: var(--sf-surface); border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); padding: 1.5rem; }
.sf-detail-header { margin-bottom: 1.25rem; }
.sf-detail-title { font-weight: 700; font-size: 1.25rem; }
.sf-detail-id { color: var(--sf-secondary); font-size: 0.875rem; }

.sf-detail-field { display: flex; flex-direction: column; gap: 0.125rem; padding: var(--sf-density-padding) 0; border-bottom: 1px solid var(--sf-background); }
.sf-detail-label { font-weight: 600; font-size: 0.875rem; color: var(--sf-secondary); }
.sf-detail-value { font-size: 1rem; }

/* Detail — split layout */
.sf-detail[data-detail-style="split"] .sf-detail-fields { display: grid; grid-template-columns: 12rem 1fr; gap: 0; }
.sf-detail[data-detail-style="split"] .sf-detail-field { flex-direction: row; align-items: baseline; }
.sf-detail[data-detail-style="split"] .sf-detail-label { min-width: 12rem; }

/* Detail — tabbed layout */
.sf-tab-group { display: flex; gap: 0; border-bottom: 2px solid var(--sf-surface); margin-bottom: 1rem; }
.sf-tab-radio { display: none; }
.sf-tab-label { padding: 0.5rem 1rem; cursor: pointer; border-bottom: 2px solid transparent; margin-bottom: -2px; }
.sf-tab-radio:checked + .sf-tab-label { border-bottom-color: var(--sf-primary); font-weight: 600; }
.sf-tab-content { display: none; }
.sf-tab-radio:checked + .sf-tab-label + .sf-tab-content { display: block; }

/* Forms */
.sf-form { display: flex; flex-direction: column; gap: 1rem; }
.sf-form-group { display: flex; flex-direction: column; gap: 0.25rem; }
.sf-form-label { font-weight: 600; font-size: 0.875rem; }
.sf-form-input { padding: 0.5rem; border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); font-family: inherit; font-size: 1rem; background: var(--sf-background); color: var(--sf-text); }
.sf-form-input:focus { outline: 2px solid var(--sf-primary); outline-offset: -1px; }
.sf-form-textarea { resize: vertical; min-height: 6rem; }
.sf-form-select { appearance: auto; }
.sf-form-checkbox { width: 1.25rem; height: 1.25rem; accent-color: var(--sf-primary); }

/* Buttons */
.sf-btn { display: inline-flex; align-items: center; gap: 0.375rem; padding: 0.5rem 1rem; border: none; border-radius: var(--sf-border-radius); font-family: inherit; font-size: 0.875rem; font-weight: 500; cursor: pointer; text-decoration: none; transition: opacity 0.15s; }
.sf-btn:hover { opacity: 0.85; }
.sf-btn-primary { background: var(--sf-primary); color: #fff; }
.sf-btn-secondary { background: var(--sf-surface); color: var(--sf-text); border: 1px solid var(--sf-secondary); }
.sf-btn-danger { background: var(--sf-error); color: #fff; }
.sf-btn-sm { padding: 0.25rem 0.625rem; font-size: 0.8125rem; }
.sf-btn-xs { padding: 0.125rem 0.5rem; font-size: 0.75rem; }

/* Pagination */
.sf-pagination { display: flex; justify-content: space-between; align-items: center; padding: var(--sf-density-padding); font-size: 0.875rem; }
.sf-pagination-info { color: var(--sf-secondary); }

/* Alerts */
.sf-alert { padding: 0.75rem 1rem; border-radius: var(--sf-border-radius); margin-bottom: 1rem; }
.sf-alert-error { background: color-mix(in srgb, var(--sf-error) 10%, transparent); border: 1px solid var(--sf-error); color: var(--sf-error); }

/* Empty state */
.sf-empty { text-align: center; padding: 3rem 1rem; color: var(--sf-secondary); }

/* Breadcrumbs */
.sf-breadcrumbs { display: flex; gap: 0.25rem; font-size: 0.875rem; color: var(--sf-secondary); margin-bottom: 1rem; }
.sf-breadcrumbs a { color: var(--sf-primary); text-decoration: none; }
.sf-breadcrumbs a:hover { text-decoration: underline; }
.sf-breadcrumbs-sep::before { content: "/"; margin: 0 0.25rem; }

/* Page header */
.sf-page-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 1.5rem; }
.sf-page-title { font-weight: 700; font-size: 1.5rem; }
.sf-page-subtitle { font-size: 0.875rem; color: var(--sf-secondary); }

/* Widget — Status Badge */
.sf-badge { display: inline-block; padding: 0.125rem 0.5rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 600; text-transform: capitalize; }
.sf-badge-success { background: color-mix(in srgb, #10B981 15%, transparent); color: #059669; }
.sf-badge-error { background: color-mix(in srgb, #EF4444 15%, transparent); color: #DC2626; }
.sf-badge-warning { background: color-mix(in srgb, #F59E0B 15%, transparent); color: #D97706; }
.sf-badge-info { background: color-mix(in srgb, #3B82F6 15%, transparent); color: #2563EB; }
.sf-badge-neutral { background: color-mix(in srgb, #6B7280 15%, transparent); color: #4B5563; }

/* Widget — Progress */
.sf-progress { width: 100%; background: var(--sf-surface); border-radius: var(--sf-border-radius); overflow: hidden; height: 1.25rem; border: 1px solid var(--sf-secondary); }
.sf-progress-bar { height: 100%; background: var(--sf-primary); color: #fff; font-size: 0.6875rem; font-weight: 600; display: flex; align-items: center; justify-content: center; min-width: 2rem; transition: width 0.3s ease; }

/* Widget — Relative Time */
.sf-relative-time { color: var(--sf-secondary); font-size: 0.875rem; }

/* Widget — Count Badge */
.sf-count-badge { display: inline-flex; align-items: center; justify-content: center; min-width: 1.5rem; height: 1.5rem; padding: 0 0.375rem; border-radius: 9999px; background: var(--sf-primary); color: #fff; font-size: 0.75rem; font-weight: 700; }

/* Widget — Link / Email / Phone */
.sf-link, .sf-email, .sf-phone { color: var(--sf-primary); text-decoration: none; }
.sf-link:hover, .sf-email:hover, .sf-phone:hover { text-decoration: underline; }

/* Widget — Color Swatch */
.sf-color-swatch { display: inline-block; width: 1rem; height: 1rem; border-radius: 0.125rem; border: 1px solid var(--sf-secondary); vertical-align: middle; }

/* Widget — Tags */
.sf-tags { display: flex; flex-wrap: wrap; gap: 0.25rem; }
.sf-tag { display: inline-block; padding: 0.0625rem 0.375rem; border-radius: var(--sf-border-radius); background: var(--sf-surface); border: 1px solid var(--sf-secondary); font-size: 0.75rem; }

/* Widget — Image Thumb */
.sf-image-thumb { max-width: 4rem; max-height: 4rem; border-radius: var(--sf-border-radius); object-fit: cover; }

/* Widget — Code */
.sf-code { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 0.8125rem; background: var(--sf-surface); padding: 0.125rem 0.375rem; border-radius: 0.25rem; }

/* Widget — Markdown */
.sf-markdown { line-height: 1.5; }

/* Kanban Board */
.sf-kanban { display: flex; gap: 1rem; overflow-x: auto; padding-bottom: 1rem; min-height: 400px; }
.sf-kanban-column { flex: 0 0 280px; background: var(--sf-surface); border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); display: flex; flex-direction: column; max-height: 80vh; }
.sf-kanban-header { display: flex; justify-content: space-between; align-items: center; padding: 0.75rem 1rem; border-bottom: 1px solid var(--sf-secondary); }
.sf-kanban-header-label { font-weight: 600; font-size: 0.875rem; }
.sf-kanban-count { font-size: 0.75rem; color: var(--sf-secondary); }
.sf-kanban-body { flex: 1; overflow-y: auto; padding: 0.5rem; display: flex; flex-direction: column; gap: 0.5rem; }
.sf-kanban-card { background: var(--sf-background); border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); padding: 0.75rem; cursor: grab; transition: box-shadow 0.15s, opacity 0.15s; }
.sf-kanban-card:hover { box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1); }
.sf-kanban-card.dragging { opacity: 0.5; }
.sf-kanban-column.drag-over { border-color: var(--sf-primary); border-width: 2px; }
.sf-kanban-card-title { font-weight: 600; font-size: 0.875rem; margin-bottom: 0.25rem; }
.sf-kanban-card-title a { color: inherit; text-decoration: none; }
.sf-kanban-card-title a:hover { color: var(--sf-primary); }
.sf-kanban-card-fields { font-size: 0.8125rem; color: var(--sf-secondary); }

/* Login Page */
.sf-login-page { display: flex; align-items: center; justify-content: center; min-height: 100vh; background: var(--sf-background); }
.sf-login-card { background: var(--sf-surface); border: 1px solid var(--sf-secondary); border-radius: var(--sf-border-radius); padding: 2rem; width: 100%; max-width: 24rem; }
.sf-login-logo { max-height: 3rem; margin-bottom: 1rem; }
.sf-login-title { font-weight: 700; font-size: 1.5rem; color: var(--sf-primary); margin: 0 0 0.25rem 0; }
.sf-login-subtitle { color: var(--sf-secondary); font-size: 0.875rem; margin: 0 0 1.5rem 0; }
.sf-login-error { margin-bottom: 1rem; }
.sf-login-form { display: flex; flex-direction: column; gap: 1rem; }
.sf-login-submit { width: 100%; justify-content: center; margin-top: 0.5rem; }
.sf-login-demo { margin-top: 1.5rem; padding-top: 1rem; border-top: 1px solid var(--sf-secondary); }
.sf-login-demo-title { font-size: 0.75rem; font-weight: 600; color: var(--sf-secondary); text-transform: uppercase; letter-spacing: 0.05em; margin: 0 0 0.5rem 0; }
.sf-login-demo-table { width: 100%; font-size: 0.75rem; border-collapse: collapse; }
.sf-login-demo-table th { text-align: left; padding: 0.25rem 0.5rem; color: var(--sf-secondary); font-weight: 600; border-bottom: 1px solid var(--sf-secondary); }
.sf-login-demo-table td { padding: 0.25rem 0.5rem; border-bottom: 1px solid var(--sf-background); }

/* Nav User Indicator */
.sf-nav-spacer { flex: 1; }
.sf-nav[data-nav-style="sidebar"] .sf-nav-spacer { flex: 1; }
.sf-nav[data-nav-style="topnav"] .sf-nav-spacer { flex: 1; }
.sf-nav-user { display: flex; flex-direction: column; gap: 0.125rem; padding: 0.5rem 0.75rem; font-size: 0.8125rem; border-top: 1px solid var(--sf-secondary); }
.sf-nav[data-nav-style="topnav"] .sf-nav-user { flex-direction: row; align-items: center; gap: 0.5rem; border-top: none; border-left: 1px solid var(--sf-secondary); padding: 0 0 0 1rem; }
.sf-nav-username { font-weight: 600; }
.sf-nav-roles { font-size: 0.75rem; color: var(--sf-secondary); }

/* Error Page */
.sf-error-page { display: flex; flex-direction: column; align-items: center; justify-content: center; min-height: 60vh; text-align: center; padding: 2rem; }
.sf-error-code { font-size: 4rem; font-weight: 700; color: var(--sf-error); margin-bottom: 0.5rem; }
.sf-error-message { font-size: 1.125rem; color: var(--sf-secondary); margin-bottom: 1.5rem; max-width: 30rem; }

/* Filter Pills */
.sf-filters { display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 1rem; }
.sf-filter-group { display: flex; align-items: center; gap: 0.25rem; flex-wrap: wrap; }
.sf-filter-label { font-size: 0.75rem; font-weight: 600; color: var(--sf-secondary); margin-right: 0.25rem; }
.sf-filter-pill { display: inline-block; padding: 0.125rem 0.5rem; border-radius: 9999px; font-size: 0.75rem; text-decoration: none; color: var(--sf-text); background: var(--sf-surface); border: 1px solid var(--sf-secondary); cursor: pointer; transition: background 0.15s; }
.sf-filter-pill:hover { background: var(--sf-background); }
.sf-filter-pill-active { background: var(--sf-primary); color: #fff; border-color: var(--sf-primary); }
.sf-filter-pill-active:hover { opacity: 0.85; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn generate_css_includes_custom_properties() {
        let theme = Theme::default();
        let css = generate_css(&theme);
        assert!(css.contains(":root {"));
        assert!(css.contains("--sf-primary: #3B82F6;"));
    }

    #[test]
    fn generate_css_includes_base_styles() {
        let theme = Theme::default();
        let css = generate_css(&theme);
        assert!(css.contains(".sf-app"));
        assert!(css.contains(".sf-nav"));
        assert!(css.contains(".sf-btn"));
        assert!(css.contains(".sf-entity-list"));
    }

    #[test]
    fn generate_css_includes_custom_css() {
        let theme = Theme {
            custom_css: Some(".my-class { color: red; }".to_string()),
            ..Theme::default()
        };
        let css = generate_css(&theme);
        assert!(css.contains(".my-class { color: red; }"));
    }

    #[test]
    fn sanitize_css_strips_import() {
        let input = "@import url('evil.css');\n.safe { color: red; }";
        let result = sanitize_css(input);
        assert!(!result.contains("@import"));
        assert!(result.contains(".safe { color: red; }"));
    }

    #[test]
    fn sanitize_css_strips_url() {
        let input = ".bg { background: url('http://evil.com/img.png'); }";
        let result = sanitize_css(input);
        assert!(!result.contains("url("));
    }

    #[test]
    fn sanitize_css_strips_javascript() {
        let input = ".xss { background: javascript:alert(1); }";
        let result = sanitize_css(input);
        assert!(!result.contains("javascript:"));
    }

    #[test]
    fn sanitize_css_strips_expression() {
        let input = ".ie { width: expression(document.body.clientWidth); }";
        let result = sanitize_css(input);
        assert!(!result.contains("expression("));
    }

    #[test]
    fn sanitize_css_strips_behavior() {
        let input = ".htc { behavior: url(evil.htc); }";
        let result = sanitize_css(input);
        assert!(!result.contains("behavior:"));
    }

    #[test]
    fn sanitize_css_preserves_safe_css() {
        let input = ".sf-custom {\n    color: #333;\n    font-size: 14px;\n    border: 1px solid #ccc;\n}";
        let result = sanitize_css(input);
        assert!(result.contains("color: #333;"));
        assert!(result.contains("font-size: 14px;"));
    }
}
