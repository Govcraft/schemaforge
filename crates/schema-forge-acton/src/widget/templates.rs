use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

// ---------------------------------------------------------------------------
// List variants
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Detail variants
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Form (no theme variants — forms don't change by theme)
// ---------------------------------------------------------------------------

/// Widget entity form — bare fragment for create/edit forms.
#[derive(serde::Serialize)]
pub struct WidgetEntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub url_prefix: String,
}
