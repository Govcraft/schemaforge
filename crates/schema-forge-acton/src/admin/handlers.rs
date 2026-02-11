use acton_service::prelude::HtmlTemplate;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use schema_forge_backend::entity::Entity;
use schema_forge_core::migration::DiffEngine;
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldName, FieldType, SchemaDefinition, SchemaName,
};

use crate::state::ForgeState;

use super::error::AdminError;
use super::form::{form_to_entity_fields, form_to_schema_definition};
use super::templates::{
    DashboardTemplate, DslPreviewFragment, EntityDetailTemplate, EntityFormTemplate,
    EntityListTemplate, EntityTableBodyFragment, FieldEditorRowFragment, RelationOptionsFragment,
    SchemaDetailTemplate, SchemaEditorTemplate, TypeConstraintsFragment,
};
use super::views::{
    DashboardEntry, EntityView, FieldEditorRow, FieldView, MigrationPreviewView, PaginationView,
    SchemaEditorView, SchemaGraphView, SchemaView,
};

/// Query params for entity list pagination.
#[derive(Debug, serde::Deserialize, Default)]
pub struct PaginationParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Helper: get sorted schema names for sidebar nav.
async fn schema_names(state: &ForgeState) -> Vec<String> {
    let mut names: Vec<String> = state
        .registry
        .list()
        .await
        .iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    names.sort();
    names
}

/// Resolve relation display values for a set of entities.
///
/// Scans the schema for relation fields, collects referenced entity IDs,
/// fetches those entities from the backend, and returns a map from
/// entity ID → display value string.
async fn resolve_ref_display(
    state: &ForgeState,
    schema: &SchemaDefinition,
    entities: &[Entity],
) -> std::collections::HashMap<String, String> {
    let mut ref_display = std::collections::HashMap::new();

    // Collect (target_schema_name, [entity_ids]) for each relation field
    let mut targets: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for field in &schema.fields {
        let target_name = match &field.field_type {
            FieldType::Relation { target, .. } => target.as_str().to_string(),
            _ => continue,
        };

        for entity in entities {
            if let Some(val) = entity.field(field.name.as_str()) {
                match val {
                    DynamicValue::Ref(id) => {
                        targets
                            .entry(target_name.clone())
                            .or_default()
                            .push(id.as_str().to_string());
                    }
                    DynamicValue::RefArray(ids) => {
                        for id in ids {
                            targets
                                .entry(target_name.clone())
                                .or_default()
                                .push(id.as_str().to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // For each target schema, fetch the entities and extract display values
    for (target_name, ids) in &targets {
        let target_schema = match state.registry.get(target_name).await {
            Some(s) => s,
            None => continue,
        };

        // Find display field for the target schema
        let display_field = target_schema.annotations.iter().find_map(|a| match a {
            Annotation::Display { field } => Some(field.as_str().to_string()),
            _ => None,
        });

        for id_str in ids {
            if ref_display.contains_key(id_str) {
                continue; // already resolved
            }
            let entity_id = match EntityId::parse(id_str) {
                Ok(eid) => eid,
                Err(_) => continue,
            };
            let target_sn = match SchemaName::new(target_name) {
                Ok(sn) => sn,
                Err(_) => continue,
            };
            let entity = match state.backend.get(&target_sn, &entity_id).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            let label = if let Some(ref df) = display_field {
                entity
                    .field(df)
                    .map(|v| match v {
                        DynamicValue::Text(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| id_str.clone())
            } else {
                // Fallback: first text field
                target_schema
                    .fields
                    .iter()
                    .find_map(|f| {
                        if matches!(f.field_type, FieldType::Text(_)) {
                            entity.field(f.name.as_str()).map(|v| match v {
                                DynamicValue::Text(s) => s.clone(),
                                other => other.to_string(),
                            })
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| id_str.clone())
            };
            ref_display.insert(id_str.clone(), label);
        }
    }

    ref_display
}

/// GET /admin/ — Dashboard with schema cards and entity counts.
pub async fn dashboard(
    State(state): State<ForgeState>,
) -> Result<impl IntoResponse, AdminError> {
    let schemas = state.registry.list().await;
    let names = schema_names(&state).await;

    let mut entries = Vec::new();
    for schema in &schemas {
        let query = schema_forge_core::query::Query::new(schema.id.clone()).with_limit(1);
        let result = state.backend.query(&query).await.map_err(AdminError::from)?;
        let entity_count = result.total_count.unwrap_or(result.entities.len());

        entries.push(DashboardEntry {
            schema: SchemaView::from_definition(schema),
            entity_count,
        });
    }
    entries.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));

    let graph = SchemaGraphView::from_entries(&entries, &schemas);

    Ok(HtmlTemplate::new(DashboardTemplate {
        entries,
        schema_names: names,
        graph,
    }))
}

/// GET /admin/schemas/{name} — Schema detail with field definitions.
pub async fn schema_detail(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let names = schema_names(&state).await;
    let schema = SchemaView::from_definition(&schema_def);

    Ok(HtmlTemplate::new(SchemaDetailTemplate {
        schema,
        schema_names: names,
    }))
}

/// GET /admin/schemas/{name}/entities — Paginated entity list.
pub async fn entity_list(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let limit = params.limit.unwrap_or(25);
    let offset = params.offset.unwrap_or(0);

    let query = schema_forge_core::query::Query::new(schema_def.id.clone())
        .with_limit(limit)
        .with_offset(offset);
    let result = state.backend.query(&query).await.map_err(AdminError::from)?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let schema = SchemaView::from_definition(&schema_def);
    let names = schema_names(&state).await;

    Ok(HtmlTemplate::new(EntityListTemplate {
        schema,
        entities,
        pagination,
        schema_names: names,
    }))
}

/// GET /admin/schemas/{name}/entities/_table — HTMX fragment for pagination.
pub async fn entity_table_fragment(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let limit = params.limit.unwrap_or(25);
    let offset = params.offset.unwrap_or(0);

    let query = schema_forge_core::query::Query::new(schema_def.id.clone())
        .with_limit(limit)
        .with_offset(offset);
    let result = state.backend.query(&query).await.map_err(AdminError::from)?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let schema = SchemaView::from_definition(&schema_def);

    Ok(HtmlTemplate::fragment(EntityTableBodyFragment {
        schema,
        entities,
        pagination,
    }))
}

/// GET /admin/schemas/{name}/entities/new — Create form.
pub async fn entity_create_form(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(FieldView::from_definition)
        .collect();
    let schema = SchemaView::from_definition(&schema_def);
    let names = schema_names(&state).await;

    Ok(HtmlTemplate::new(EntityFormTemplate {
        schema,
        fields,
        entity_id: None,
        schema_names: names,
        errors: vec![],
    }))
}

/// POST /admin/schemas/{name}/entities — Create entity from form.
pub async fn entity_create(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let schema_name = SchemaName::new(&name).map_err(|_| AdminError::SchemaNotFound {
        name: name.clone(),
    })?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = Entity::new(schema_name, fields);
            let created = state.backend.create(&entity).await.map_err(AdminError::from)?;
            Ok(Redirect::to(&format!(
                "/admin/schemas/{}/entities/{}",
                name,
                created.id.as_str()
            ))
            .into_response())
        }
        Err(errors) => {
            // Re-render form with errors
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);
            let names = schema_names(&state).await;

            Ok(HtmlTemplate::new(EntityFormTemplate {
                schema,
                fields,
                entity_id: None,
                schema_names: names,
                errors,
            })
            .with_status(StatusCode::UNPROCESSABLE_ENTITY)
            .into_response())
        }
    }
}

/// GET /admin/schemas/{name}/entities/{id} — Entity detail view.
pub async fn entity_detail(
    State(state): State<ForgeState>,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let schema_name = SchemaName::new(&name).map_err(|_| AdminError::SchemaNotFound {
        name: name.clone(),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| AdminError::EntityNotFound {
        schema: name.clone(),
        entity_id: id.clone(),
    })?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(AdminError::from)?;

    let ref_display = resolve_ref_display(&state, &schema_def, std::slice::from_ref(&entity)).await;
    let entity_view = EntityView::from_entity_with_refs(&entity, &schema_def, &ref_display);
    let schema = SchemaView::from_definition(&schema_def);
    let names = schema_names(&state).await;

    Ok(HtmlTemplate::new(EntityDetailTemplate {
        schema,
        entity: entity_view,
        schema_names: names,
    }))
}

/// GET /admin/schemas/{name}/entities/{id}/edit — Edit form.
pub async fn entity_edit_form(
    State(state): State<ForgeState>,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let schema_name = SchemaName::new(&name).map_err(|_| AdminError::SchemaNotFound {
        name: name.clone(),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| AdminError::EntityNotFound {
        schema: name.clone(),
        entity_id: id.clone(),
    })?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(AdminError::from)?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(|f| {
            let value = entity.field(f.name.as_str());
            FieldView::from_definition_with_value(f, value)
        })
        .collect();
    let schema = SchemaView::from_definition(&schema_def);
    let names = schema_names(&state).await;

    Ok(HtmlTemplate::new(EntityFormTemplate {
        schema,
        fields,
        entity_id: Some(id),
        schema_names: names,
        errors: vec![],
    }))
}

/// PUT /admin/schemas/{name}/entities/{id} — Update entity from form.
pub async fn entity_update(
    State(state): State<ForgeState>,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let schema_name = SchemaName::new(&name).map_err(|_| AdminError::SchemaNotFound {
        name: name.clone(),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| AdminError::EntityNotFound {
        schema: name.clone(),
        entity_id: id.clone(),
    })?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = Entity::with_id(entity_id, schema_name, fields);
            state.backend.update(&entity).await.map_err(AdminError::from)?;
            Ok(Redirect::to(&format!(
                "/admin/schemas/{}/entities/{}",
                name, id
            ))
            .into_response())
        }
        Err(errors) => {
            // Re-render form with errors
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);
            let names = schema_names(&state).await;

            Ok(HtmlTemplate::new(EntityFormTemplate {
                schema,
                fields,
                entity_id: Some(id),
                schema_names: names,
                errors,
            })
            .with_status(StatusCode::UNPROCESSABLE_ENTITY)
            .into_response())
        }
    }
}

/// DELETE /admin/schemas/{name}/entities/{id} — Delete entity.
pub async fn entity_delete(
    State(state): State<ForgeState>,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_name = SchemaName::new(&name).map_err(|_| AdminError::SchemaNotFound {
        name: name.clone(),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| AdminError::EntityNotFound {
        schema: name.clone(),
        entity_id: id.clone(),
    })?;

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(AdminError::from)?;

    // Return empty body — HTMX will remove the row
    Ok(StatusCode::OK)
}

/// GET /admin/schemas/{target}/relation-options/{field} — Lazy-load relation options.
pub async fn relation_options(
    State(state): State<ForgeState>,
    Path((target, _field)): Path<(String, String)>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&target)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound {
            name: target.clone(),
        })?;

    let query =
        schema_forge_core::query::Query::new(schema_def.id.clone()).with_limit(100);
    let result = state.backend.query(&query).await.map_err(AdminError::from)?;

    // Find display field
    let display_field = schema_def.annotations.iter().find_map(|a| match a {
        Annotation::Display { field } => Some(field.as_str().to_string()),
        _ => None,
    });

    let options: Vec<(String, String)> = result
        .entities
        .iter()
        .map(|entity| {
            let id = entity.id.as_str().to_string();
            let label = if let Some(ref df) = display_field {
                entity
                    .field(df)
                    .map(|v| match v {
                        DynamicValue::Text(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| id.clone())
            } else {
                // Try first text field
                schema_def
                    .fields
                    .iter()
                    .find_map(|f| {
                        if matches!(f.field_type, FieldType::Text(_)) {
                            entity.field(f.name.as_str()).map(|v| match v {
                                DynamicValue::Text(s) => s.clone(),
                                other => other.to_string(),
                            })
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| id.clone())
            };
            (id, label)
        })
        .collect();

    Ok(HtmlTemplate::fragment(RelationOptionsFragment { options }))
}

// ---------------------------------------------------------------------------
// Schema CRUD handlers
// ---------------------------------------------------------------------------

/// GET /admin/schemas/new — Create schema form.
pub async fn schema_create_form(
    State(state): State<ForgeState>,
) -> Result<impl IntoResponse, AdminError> {
    let names = schema_names(&state).await;
    Ok(HtmlTemplate::new(SchemaEditorTemplate {
        editor: SchemaEditorView::new_empty(),
        schema_names: names,
        errors: vec![],
    }))
}

/// POST /admin/schemas — Create schema from form.
pub async fn schema_create(
    State(state): State<ForgeState>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, AdminError> {
    match form_to_schema_definition(&form_data, None) {
        Ok(schema_def) => {
            let name = schema_def.name.as_str().to_string();

            // Apply creation migration
            let plan = DiffEngine::create_new(&schema_def);
            state
                .backend
                .apply_migration(&schema_def.name, &plan.steps)
                .await
                .map_err(AdminError::from)?;

            // Store metadata
            state
                .backend
                .store_schema_metadata(&schema_def)
                .await
                .map_err(AdminError::from)?;

            // Update registry
            state.registry.insert(name.clone(), schema_def).await;

            Ok(Redirect::to(&format!("/admin/schemas/{name}")).into_response())
        }
        Err(errors) => {
            let names = schema_names(&state).await;
            // Re-populate editor from form data best-effort
            let editor = editor_from_form_data(&form_data, false);
            Ok(HtmlTemplate::new(SchemaEditorTemplate {
                editor,
                schema_names: names,
                errors,
            })
            .with_status(StatusCode::UNPROCESSABLE_ENTITY)
            .into_response())
        }
    }
}

/// GET /admin/schemas/{name}/edit — Edit schema form.
pub async fn schema_edit_form(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    let names = schema_names(&state).await;
    let editor = SchemaEditorView::from_definition(&schema_def);

    Ok(HtmlTemplate::new(SchemaEditorTemplate {
        editor,
        schema_names: names,
        errors: vec![],
    }))
}

/// POST /admin/schemas/{name} — Update schema from form.
pub async fn schema_update(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, AdminError> {
    let old_schema = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| AdminError::SchemaNotFound { name: name.clone() })?;

    match form_to_schema_definition(&form_data, Some(old_schema.id.clone())) {
        Ok(new_schema) => {
            // Diff and apply migration, detecting renames from form data
            let renames = extract_renames(&form_data);
            let plan = DiffEngine::diff_with_renames(&old_schema, &new_schema, &renames);
            if !plan.steps.is_empty() {
                state
                    .backend
                    .apply_migration(&new_schema.name, &plan.steps)
                    .await
                    .map_err(AdminError::from)?;
            }

            // Store updated metadata
            state
                .backend
                .store_schema_metadata(&new_schema)
                .await
                .map_err(AdminError::from)?;

            // Update registry
            let new_name = new_schema.name.as_str().to_string();
            state.registry.insert(new_name.clone(), new_schema).await;

            Ok(Redirect::to(&format!("/admin/schemas/{new_name}")).into_response())
        }
        Err(errors) => {
            let names = schema_names(&state).await;
            let editor = editor_from_form_data(&form_data, true);
            Ok(HtmlTemplate::new(SchemaEditorTemplate {
                editor,
                schema_names: names,
                errors,
            })
            .with_status(StatusCode::UNPROCESSABLE_ENTITY)
            .into_response())
        }
    }
}

/// DELETE /admin/schemas/{name} — Delete schema.
pub async fn schema_delete(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    state.registry.remove(&name).await;

    // Return redirect as full page response
    Ok(Redirect::to("/admin/"))
}

/// POST /admin/schemas/_preview — DSL + migration preview fragment.
pub async fn schema_preview(
    State(state): State<ForgeState>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<impl IntoResponse, AdminError> {
    // Try to parse the form data best-effort
    match form_to_schema_definition(&form_data, None) {
        Ok(schema_def) => {
            let dsl_text = schema_forge_dsl::printer::print(&schema_def);

            // Check if we're in edit mode
            let existing_name = form_data
                .iter()
                .find(|(k, _)| k == "_existing_schema_name")
                .map(|(_, v)| v.clone());

            let migration = if let Some(ref ename) = existing_name {
                if let Some(old_schema) = state.registry.get(ename).await {
                    let renames = extract_renames(&form_data);
                    let plan =
                        DiffEngine::diff_with_renames(&old_schema, &schema_def, &renames);
                    Some(MigrationPreviewView::from_plan(&plan))
                } else {
                    None
                }
            } else {
                None
            };

            Ok(HtmlTemplate::fragment(DslPreviewFragment {
                dsl_text,
                errors: vec![],
                migration,
            }))
        }
        Err(errors) => Ok(HtmlTemplate::fragment(DslPreviewFragment {
            dsl_text: String::new(),
            errors,
            migration: None,
        })),
    }
}

/// Query params for type constraints fragment.
#[derive(Debug, serde::Deserialize, Default)]
pub struct TypeConstraintParams {
    pub index: Option<usize>,
}

/// GET /admin/schemas/_field-row/{index} — New empty field row.
pub async fn field_row_fragment(
    Path(index): Path<usize>,
) -> impl IntoResponse {
    HtmlTemplate::fragment(FieldEditorRowFragment {
        field: FieldEditorRow::empty(index),
    })
}

/// GET /admin/schemas/_type-constraints/{field_type} — Type-specific constraint inputs.
pub async fn type_constraints_fragment(
    Path(field_type): Path<String>,
    Query(params): Query<TypeConstraintParams>,
) -> impl IntoResponse {
    HtmlTemplate::fragment(TypeConstraintsFragment {
        field_type,
        index: params.index.unwrap_or(0),
    })
}

/// Extract rename pairs from form data.
///
/// Scans for `field_N_old_name` and `field_N_name` pairs. If both are valid
/// `FieldName`s and differ, returns `(old_name, new_name)`.
fn extract_renames(form_data: &[(String, String)]) -> Vec<(FieldName, FieldName)> {
    let mut form_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (key, value) in form_data {
        form_map.insert(key.clone(), value.clone());
    }

    let mut renames = Vec::new();
    // Find all field indices that have an old_name
    for (key, old_val) in &form_map {
        if let Some(rest) = key.strip_prefix("field_") {
            if let Some(idx_str) = rest.strip_suffix("_old_name") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if let Some(new_val) = form_map.get(&format!("field_{idx}_name")) {
                        if old_val != new_val {
                            if let (Ok(old_name), Ok(new_name)) =
                                (FieldName::new(old_val), FieldName::new(new_val))
                            {
                                renames.push((old_name, new_name));
                            }
                        }
                    }
                }
            }
        }
    }
    renames
}

/// Reconstruct a `SchemaEditorView` from raw form data for re-rendering on error.
fn editor_from_form_data(form_data: &[(String, String)], is_edit: bool) -> SchemaEditorView {
    let mut form_map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (key, value) in form_data {
        form_map.insert(key.clone(), value.clone());
    }

    let schema_name = form_map.get("schema_name").cloned().unwrap_or_default();
    let version = form_map.get("version").cloned().unwrap_or_default();
    let display_field = form_map.get("display_field").cloned().unwrap_or_default();

    // Discover field indices
    let mut field_indices: Vec<usize> = Vec::new();
    for key in form_map.keys() {
        if let Some(rest) = key.strip_prefix("field_") {
            if let Some(idx_str) = rest.strip_suffix("_name") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if !field_indices.contains(&idx) {
                        field_indices.push(idx);
                    }
                }
            }
        }
    }
    field_indices.sort_unstable();

    let fields: Vec<FieldEditorRow> = field_indices
        .iter()
        .map(|idx| {
            let prefix = format!("field_{idx}_");
            FieldEditorRow {
                index: *idx,
                name: form_map.get(&format!("{prefix}name")).cloned().unwrap_or_default(),
                old_name: form_map.get(&format!("{prefix}old_name")).cloned(),
                field_type: form_map.get(&format!("{prefix}type")).cloned().unwrap_or_else(|| "text".to_string()),
                required: form_map.get(&format!("{prefix}required")).is_some_and(|v| v == "true" || v == "on"),
                indexed: form_map.get(&format!("{prefix}indexed")).is_some_and(|v| v == "true" || v == "on"),
                default_enabled: false,
                default_value: String::new(),
                text_max_length: form_map.get(&format!("{prefix}text_max_length")).and_then(|v| v.parse().ok()),
                integer_min: form_map.get(&format!("{prefix}integer_min")).and_then(|v| v.parse().ok()),
                integer_max: form_map.get(&format!("{prefix}integer_max")).and_then(|v| v.parse().ok()),
                float_precision: form_map.get(&format!("{prefix}float_precision")).and_then(|v| v.parse().ok()),
                enum_variants: form_map.get(&format!("{prefix}enum_variants")).cloned().unwrap_or_default(),
                relation_target: form_map.get(&format!("{prefix}relation_target")).cloned().unwrap_or_default(),
                relation_cardinality: form_map.get(&format!("{prefix}relation_cardinality")).cloned().unwrap_or_else(|| "one".to_string()),
            }
        })
        .collect();

    let existing_name = if is_edit { Some(schema_name.clone()) } else { None };

    SchemaEditorView {
        schema_name,
        version,
        display_field,
        fields,
        is_edit,
        existing_name,
    }
}
