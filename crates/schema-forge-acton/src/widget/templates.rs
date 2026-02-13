use acton_service::prelude::Template;

use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

// ---------------------------------------------------------------------------
// List variants
// ---------------------------------------------------------------------------

/// Widget entity table — table layout (default).
#[derive(Template)]
#[template(path = "forge/entity_list_table.html")]
pub struct WidgetEntityListTableTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Widget entity cards — card grid layout.
#[derive(Template)]
#[template(path = "forge/entity_list_cards.html")]
pub struct WidgetEntityListCardsTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Widget entity compact — dense single-line layout.
#[derive(Template)]
#[template(path = "forge/entity_list_compact.html")]
pub struct WidgetEntityListCompactTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Backwards-compatible alias — uses table layout.
#[derive(Template)]
#[template(path = "forge/entity_table.html")]
pub struct WidgetEntityTableTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

// ---------------------------------------------------------------------------
// Detail variants
// ---------------------------------------------------------------------------

/// Widget entity detail — full vertical layout (default).
#[derive(Template)]
#[template(path = "organisms/entity_detail_full.html")]
pub struct WidgetEntityDetailFullTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Widget entity detail — two-column split layout.
#[derive(Template)]
#[template(path = "organisms/entity_detail_split.html")]
pub struct WidgetEntityDetailSplitTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Widget entity detail — tabbed layout.
#[derive(Template)]
#[template(path = "organisms/entity_detail_tabbed.html")]
pub struct WidgetEntityDetailTabbedTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

/// Backwards-compatible alias — uses full layout.
#[derive(Template)]
#[template(path = "forge/entity_detail.html")]
pub struct WidgetEntityDetailTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub url_prefix: String,
}

// ---------------------------------------------------------------------------
// Form (no theme variants — forms don't change by theme)
// ---------------------------------------------------------------------------

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
