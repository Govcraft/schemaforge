use crate::views::{EntityView, FieldView, FilterField, KanbanColumn, PaginationView, SchemaView};

use super::auth::SiteUserView;

// ---------------------------------------------------------------------------
// Shared heading / navigation types
// ---------------------------------------------------------------------------

/// A single action button in a page heading.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeadingAction {
    pub url: String,
    pub label: String,
    pub class: String,
}

/// A breadcrumb item.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BreadcrumbItem {
    pub label: String,
    pub url: Option<String>,
}

/// Schema entry for navigation sidebar.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NavSchemaEntry {
    pub url_name: String,
    pub label: String,
    /// Display string for entity count badge (e.g. "5", "12", "20+"). None = no badge.
    pub entity_count: Option<String>,
}

/// A stat item for standalone stats display components.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatItem {
    pub label: String,
    pub value: String,
    pub unit: Option<String>,
    pub trend_value: Option<String>,
    /// "up" or "down"
    pub trend_direction: Option<String>,
    pub previous_value: Option<String>,
    pub icon_svg: Option<String>,
    pub link_url: Option<String>,
    pub link_label: Option<String>,
}

/// Schema card for dashboard — one card per aggregate widget.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DashboardCard {
    pub url_name: String,
    pub label: String,
    pub widget_label: String,
    pub display_value: String,
}

// ---------------------------------------------------------------------------
// Site (full-page) templates
// ---------------------------------------------------------------------------

/// Site login page — standalone, no base.html.
#[derive(serde::Serialize)]
pub struct SiteLoginTemplate {
    pub error: Option<String>,
}

/// Site dashboard page.
#[derive(serde::Serialize)]
pub struct SiteDashboardTemplate {
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema_cards: Vec<DashboardCard>,
    pub current_user: Option<SiteUserView>,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
    pub stats: Vec<StatItem>,
}

/// Site entity list page.
#[derive(serde::Serialize)]
pub struct SiteEntityListTemplate {
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
    pub filter_fields: Vec<FilterField>,
    pub current_user: Option<SiteUserView>,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
}

/// Site entity list body fragment (for HTMX pagination).
#[derive(serde::Serialize)]
pub struct SiteEntityListBodyTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
    pub filter_fields: Vec<FilterField>,
}

/// Site kanban entity list page.
#[derive(serde::Serialize)]
pub struct SiteEntityListKanbanTemplate {
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub columns: Vec<KanbanColumn>,
    pub kanban_field: String,
    pub current_user: Option<SiteUserView>,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
}

/// Site entity form page (create/edit).
#[derive(serde::Serialize)]
pub struct SiteEntityFormTemplate {
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub current_user: Option<SiteUserView>,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
}

/// Site entity detail page.
#[derive(serde::Serialize)]
pub struct SiteEntityDetailTemplate {
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub entity: EntityView,
    pub detail_style: String,
    pub current_user: Option<SiteUserView>,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
}

// ---------------------------------------------------------------------------
// Widget (bare HTMX fragment) templates — unchanged
// ---------------------------------------------------------------------------

// List variants

/// Widget entity table — table layout (default).
#[derive(serde::Serialize)]
pub struct WidgetEntityListTableTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Widget entity cards — card grid layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityListCardsTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Widget entity compact — dense single-line layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityListCompactTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Backwards-compatible alias — uses table layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityTableTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

// Detail variants

/// Widget entity detail — full vertical layout (default).
#[derive(serde::Serialize)]
pub struct WidgetEntityDetailFullTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Widget entity detail — two-column split layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityDetailSplitTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Widget entity detail — tabbed layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityDetailTabbedTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Backwards-compatible alias — uses full layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityDetailTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

// Form

/// Widget entity form — bare fragment for create/edit forms.
#[derive(serde::Serialize)]
pub struct WidgetEntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub url_prefix: String,
}
