use acton_service::prelude::Template;

use super::views::{DashboardEntry, EntityView, FieldView, PaginationView, SchemaView};

/// Dashboard page — lists all schemas with entity counts.
#[derive(Template)]
#[template(path = "admin/dashboard.html")]
pub struct DashboardTemplate {
    pub entries: Vec<DashboardEntry>,
    pub schema_names: Vec<String>,
}

/// Schema detail page — shows field definitions.
#[derive(Template)]
#[template(path = "admin/schema_detail.html")]
pub struct SchemaDetailTemplate {
    pub schema: SchemaView,
    pub schema_names: Vec<String>,
}

/// Entity list page — paginated table of entities.
#[derive(Template)]
#[template(path = "admin/entity_list.html")]
pub struct EntityListTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub schema_names: Vec<String>,
}

/// Entity create form.
#[derive(Template)]
#[template(path = "admin/entity_form.html")]
pub struct EntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub schema_names: Vec<String>,
    pub errors: Vec<String>,
}

/// Entity detail page.
#[derive(Template)]
#[template(path = "admin/entity_detail.html")]
pub struct EntityDetailTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub schema_names: Vec<String>,
}

/// Entity table body fragment (for HTMX pagination).
#[derive(Template)]
#[template(path = "admin/fragments/entity_table_body.html")]
pub struct EntityTableBodyFragment {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
}

/// Flash message fragment.
#[derive(Template)]
#[template(path = "admin/fragments/flash_message.html")]
pub struct FlashMessageFragment {
    pub message: String,
    pub is_error: bool,
}

/// Relation options fragment — `<option>` elements for select dropdowns.
#[derive(Template)]
#[template(path = "admin/fragments/relation_options.html")]
pub struct RelationOptionsFragment {
    pub options: Vec<(String, String)>,
}
