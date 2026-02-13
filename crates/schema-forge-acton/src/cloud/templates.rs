use acton_service::prelude::Template;

use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

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
}

/// Cloud entity list body fragment (for HTMX pagination).
#[derive(Template)]
#[template(path = "cloud/fragments/entity_list_body.html")]
pub struct CloudEntityListBodyTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub list_style: String,
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
}
