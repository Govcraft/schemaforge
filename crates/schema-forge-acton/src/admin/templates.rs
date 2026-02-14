use super::auth::CurrentUserView;
use super::views::{
    DashboardEntry, EntityView, FieldEditorRow, FieldView, MigrationPreviewView, PaginationView,
    SchemaEditorView, SchemaGraphView, SchemaView,
};
use crate::shared_auth::ForgeUser;

/// Login page — standalone, no base.html.
#[derive(serde::Serialize)]
pub struct LoginTemplate {
    pub error: Option<String>,
}

/// Dashboard page — lists all schemas with entity counts.
#[derive(serde::Serialize)]
pub struct DashboardTemplate {
    pub entries: Vec<DashboardEntry>,
    pub schema_names: Vec<String>,
    pub graph: SchemaGraphView,
    pub current_user: Option<CurrentUserView>,
}

/// Schema detail page — shows field definitions.
#[derive(serde::Serialize)]
pub struct SchemaDetailTemplate {
    pub schema: SchemaView,
    pub schema_names: Vec<String>,
    pub current_user: Option<CurrentUserView>,
}

/// Entity list page — paginated table of entities.
#[derive(serde::Serialize)]
pub struct EntityListTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub schema_names: Vec<String>,
    pub current_user: Option<CurrentUserView>,
    pub url_prefix: String,
}

/// Entity create form.
#[derive(serde::Serialize)]
pub struct EntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub schema_names: Vec<String>,
    pub errors: Vec<String>,
    pub current_user: Option<CurrentUserView>,
    pub url_prefix: String,
}

/// Entity detail page.
#[derive(serde::Serialize)]
pub struct EntityDetailTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub schema_names: Vec<String>,
    pub current_user: Option<CurrentUserView>,
    pub url_prefix: String,
}

/// Entity table body fragment (for HTMX pagination).
#[derive(serde::Serialize)]
pub struct EntityTableBodyFragment {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub url_prefix: String,
}

/// Flash message fragment.
#[derive(serde::Serialize)]
pub struct FlashMessageFragment {
    pub message: String,
    pub is_error: bool,
}

/// Relation options fragment — `<option>` elements for select dropdowns.
#[derive(serde::Serialize)]
pub struct RelationOptionsFragment {
    pub options: Vec<(String, String)>,
}

/// Schema editor page — create or edit a schema.
#[derive(serde::Serialize)]
pub struct SchemaEditorTemplate {
    pub editor: SchemaEditorView,
    pub schema_names: Vec<String>,
    pub errors: Vec<String>,
    pub current_user: Option<CurrentUserView>,
}

/// Field editor row fragment — a single field row for HTMX append.
#[derive(serde::Serialize)]
pub struct FieldEditorRowFragment {
    pub field: FieldEditorRow,
}

/// Type constraints fragment — type-specific inputs swapped via HTMX.
#[derive(serde::Serialize)]
pub struct TypeConstraintsFragment {
    pub field_type: String,
    pub index: usize,
}

/// DSL preview fragment — formatted DSL text.
#[derive(serde::Serialize)]
pub struct DslPreviewFragment {
    pub dsl_text: String,
    pub errors: Vec<String>,
    pub migration: Option<MigrationPreviewView>,
}

/// Migration preview fragment.
#[derive(serde::Serialize)]
pub struct MigrationPreviewFragment {
    pub migration: MigrationPreviewView,
}

/// User management list page.
#[derive(serde::Serialize)]
pub struct UserListTemplate {
    pub users: Vec<ForgeUser>,
    pub schema_names: Vec<String>,
    pub current_user: Option<CurrentUserView>,
}

/// User create/edit form page.
#[derive(serde::Serialize)]
pub struct UserFormTemplate {
    pub is_edit: bool,
    pub username: String,
    pub display_name: String,
    pub available_roles: Vec<String>,
    pub selected_roles: Vec<String>,
    pub schema_names: Vec<String>,
    pub errors: Vec<String>,
    pub current_user: Option<CurrentUserView>,
}
