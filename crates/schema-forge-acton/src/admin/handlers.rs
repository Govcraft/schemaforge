use acton_service::prelude::HtmlTemplate;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldType, SchemaName,
};

use crate::state::ForgeState;

use super::error::AdminError;
use super::form::form_to_entity_fields;
use super::templates::{
    DashboardTemplate, EntityDetailTemplate, EntityFormTemplate, EntityListTemplate,
    EntityTableBodyFragment, RelationOptionsFragment,
    SchemaDetailTemplate,
};
use super::views::{
    DashboardEntry, EntityView, FieldView, PaginationView, SchemaView,
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

    Ok(HtmlTemplate::new(DashboardTemplate {
        entries,
        schema_names: names,
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
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity(e, &schema_def))
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
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity(e, &schema_def))
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

    let entity_view = EntityView::from_entity(&entity, &schema_def);
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
