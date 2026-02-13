use acton_service::prelude::Template;

use crate::views::{EntityView, FieldView, FilterField, KanbanColumn, PaginationView, SchemaView};

use super::auth::CloudUserView;

/// Schema entry for navigation sidebar.
#[derive(Debug, Clone)]
pub struct NavSchemaEntry {
    pub url_name: String,
    pub label: String,
}

/// Schema card for dashboard — one card per aggregate widget.
#[derive(Debug, Clone)]
pub struct DashboardCard {
    pub url_name: String,
    pub label: String,
    pub widget_label: String,
    pub display_value: String,
}

/// Cloud login page — standalone, no base.html.
#[derive(Template)]
#[template(path = "cloud/login.html")]
pub struct CloudLoginTemplate {
    pub app_title: String,
    pub logo_url: Option<String>,
    pub error: Option<String>,
}

/// Cloud dashboard page.
#[derive(Template)]
#[template(path = "cloud/dashboard.html")]
pub struct CloudDashboardTemplate {
    pub app_title: String,
    pub nav_style: String,
    pub logo_url: Option<String>,
    pub nav_schemas: Vec<NavSchemaEntry>,
    pub active_nav: String,
    pub schema_cards: Vec<DashboardCard>,
    pub current_user: Option<CloudUserView>,
}

/// Cloud entity list page.
#[derive(Template)]
#[template(path = "cloud/entity_list.html")]
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
}

/// Cloud entity list body fragment (for HTMX pagination).
#[derive(Template)]
#[template(path = "cloud/fragments/entity_list_body.html")]
pub struct CloudEntityListBodyTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
    pub filter_fields: Vec<FilterField>,
}

/// Cloud kanban entity list page.
#[derive(Template)]
#[template(path = "cloud/entity_list_kanban.html")]
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
}

/// Cloud entity form page (create/edit).
#[derive(Template)]
#[template(path = "cloud/entity_form.html")]
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
}

/// Cloud entity detail page.
#[derive(Template)]
#[template(path = "cloud/entity_detail.html")]
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
}
