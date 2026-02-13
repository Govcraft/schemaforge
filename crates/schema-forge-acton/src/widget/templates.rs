use acton_service::prelude::Template;

use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

/// Widget entity table — bare fragment for embedding entity lists.
#[derive(Template)]
#[template(path = "forge/entity_table.html")]
pub struct WidgetEntityTableTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Widget entity form — bare fragment for create/edit forms.
#[derive(Template)]
#[template(path = "forge/entity_form.html")]
pub struct WidgetEntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub url_prefix: String,
}

/// Widget entity detail — bare fragment for viewing a single entity.
#[derive(Template)]
#[template(path = "forge/entity_detail.html")]
pub struct WidgetEntityDetailTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}
