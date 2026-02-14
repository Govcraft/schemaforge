use crate::views::{EntityView, FieldView, FilterField, KanbanColumn, PaginationView, SchemaView};

use super::auth::CloudUserView;

/// Schema entry for navigation sidebar.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NavSchemaEntry {
    pub url_name: String,
    pub label: String,
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
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema_cards: Vec<DashboardCard>,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
}

/// Cloud entity list page.
#[derive(serde::Serialize)]
pub struct CloudEntityListTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
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
}

/// Cloud entity list body fragment (for HTMX pagination).
#[derive(serde::Serialize)]
pub struct CloudEntityListBodyTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
    pub filter_fields: Vec<FilterField>,
}

/// Cloud kanban entity list page.
#[derive(serde::Serialize)]
pub struct CloudEntityListKanbanTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub columns: Vec<KanbanColumn>,
    pub kanban_field: String,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
}

/// Cloud entity form page (create/edit).
#[derive(serde::Serialize)]
pub struct CloudEntityFormTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
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
}

/// Cloud entity detail page.
#[derive(serde::Serialize)]
pub struct CloudEntityDetailTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema: SchemaView,
    pub entity: EntityView,
    pub detail_style: String,
    pub current_user: Option<CloudUserView>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
}
