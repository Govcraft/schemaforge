use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

// ---------------------------------------------------------------------------
// Widget (bare HTMX fragment) templates
// ---------------------------------------------------------------------------

/// Widget entity table — table layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityListTableTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Widget entity detail — full vertical layout.
#[derive(serde::Serialize)]
pub struct WidgetEntityDetailFullTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Widget entity form — bare fragment for create/edit forms.
#[derive(serde::Serialize)]
pub struct WidgetEntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub url_prefix: String,
}
