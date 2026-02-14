use crate::views::{EntityView, FieldView, FilterField, KanbanColumn, PaginationView, SchemaView};

use super::auth::CloudUserView;

/// A single action button in a page heading.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeadingAction {
    pub url: String,
    pub label: String,
    pub class: String,
}

/// A metadata item displayed in meta-row heading variants.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeadingMetaItem {
    pub icon: String,
    pub text: String,
}

/// A stat cell displayed in the card heading variant.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeadingStatItem {
    pub label: String,
    pub value: String,
}

/// A filter tab in the filters heading variant.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeadingFilterTab {
    pub label: String,
    pub url: String,
    pub active: bool,
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

/// Team/group entry for navigation sidebar "Your teams" section.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NavTeamEntry {
    pub label: String,
    pub letter: String,
    pub url: String,
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

/// Cloud login page — standalone, no base.html.
#[derive(serde::Serialize)]
pub struct CloudLoginTemplate {
    pub app_title: String,
    pub logo_url: Option<String>,
    pub error: Option<String>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
}

/// Cloud dashboard page.
#[derive(serde::Serialize)]
pub struct CloudDashboardTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub nav_color_scheme: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub nav_teams: Vec<NavTeamEntry>,
    pub active_nav: String,
    pub schema_cards: Vec<DashboardCard>,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
    pub heading_style: String,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
    pub heading_meta: Vec<HeadingMetaItem>,
    pub heading_stats: Vec<HeadingStatItem>,
    pub heading_avatar_url: Option<String>,
    pub heading_banner_url: Option<String>,
    pub heading_filter_tabs: Vec<HeadingFilterTab>,
    pub heading_logo_url: Option<String>,
    pub stats: Vec<StatItem>,
    pub stats_style: String,
    pub stats_heading: Option<String>,
    pub card_style: String,
    pub container_style: String,
}

/// Cloud entity list page.
#[derive(serde::Serialize)]
pub struct CloudEntityListTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub nav_color_scheme: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub nav_teams: Vec<NavTeamEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
    pub filter_fields: Vec<FilterField>,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
    pub heading_style: String,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
    pub heading_meta: Vec<HeadingMetaItem>,
    pub heading_stats: Vec<HeadingStatItem>,
    pub heading_avatar_url: Option<String>,
    pub heading_banner_url: Option<String>,
    pub heading_filter_tabs: Vec<HeadingFilterTab>,
    pub heading_logo_url: Option<String>,
    pub card_style: String,
    pub container_style: String,
}

/// Cloud entity list body fragment (for HTMX pagination).
#[derive(serde::Serialize)]
pub struct CloudEntityListBodyTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
    pub filter_fields: Vec<FilterField>,
    pub card_style: String,
}

/// Cloud kanban entity list page.
#[derive(serde::Serialize)]
pub struct CloudEntityListKanbanTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub nav_color_scheme: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub nav_teams: Vec<NavTeamEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub columns: Vec<KanbanColumn>,
    pub kanban_field: String,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
    pub heading_style: String,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
    pub heading_meta: Vec<HeadingMetaItem>,
    pub heading_stats: Vec<HeadingStatItem>,
    pub heading_avatar_url: Option<String>,
    pub heading_banner_url: Option<String>,
    pub heading_filter_tabs: Vec<HeadingFilterTab>,
    pub heading_logo_url: Option<String>,
    pub card_style: String,
    pub container_style: String,
}

/// Cloud entity form page (create/edit).
#[derive(serde::Serialize)]
pub struct CloudEntityFormTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub nav_color_scheme: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub nav_teams: Vec<NavTeamEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
    pub heading_style: String,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
    pub heading_meta: Vec<HeadingMetaItem>,
    pub heading_stats: Vec<HeadingStatItem>,
    pub heading_avatar_url: Option<String>,
    pub heading_banner_url: Option<String>,
    pub heading_filter_tabs: Vec<HeadingFilterTab>,
    pub heading_logo_url: Option<String>,
    pub card_style: String,
    pub container_style: String,
}

/// Cloud entity detail page.
#[derive(serde::Serialize)]
pub struct CloudEntityDetailTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub nav_color_scheme: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub nav_teams: Vec<NavTeamEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub entity: EntityView,
    pub detail_style: String,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
    pub heading_style: String,
    pub heading_actions: Vec<HeadingAction>,
    pub breadcrumbs: Vec<BreadcrumbItem>,
    pub heading_meta: Vec<HeadingMetaItem>,
    pub heading_stats: Vec<HeadingStatItem>,
    pub heading_avatar_url: Option<String>,
    pub heading_banner_url: Option<String>,
    pub heading_filter_tabs: Vec<HeadingFilterTab>,
    pub heading_logo_url: Option<String>,
    pub card_style: String,
    pub container_style: String,
}
