use acton_service::prelude::HtmlTemplate;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schema_forge_core::types::{EntityId, SchemaName};

use crate::access::{check_schema_access, filter_entity_fields, AccessAction, FieldFilterDirection, OptionalAuth};
use crate::form::form_to_entity_fields;
use crate::shared::resolve_ref_display;
use crate::state::ForgeState;
use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

use super::error::WidgetError;
use super::templates::{
    WidgetEntityDetailTemplate, WidgetEntityFormTemplate, WidgetEntityTableTemplate,
};

/// URL prefix for widget routes: `/forge`.
const WIDGET_URL_PREFIX: &str = "/forge";

/// Query params for entity list pagination.
#[derive(Debug, serde::Deserialize, Default)]
pub struct PaginationParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /forge/{schema}/entities — Paginated entity table fragment.
pub async fn entity_list(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<impl IntoResponse, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(WidgetError::from)?;

    let limit = params.limit.unwrap_or(25);
    let offset = params.offset.unwrap_or(0);

    let query = schema_forge_core::query::Query::new(schema_def.id.clone())
        .with_limit(limit)
        .with_offset(offset);
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let schema = SchemaView::from_definition(&schema_def);

    Ok(HtmlTemplate::fragment(WidgetEntityTableTemplate {
        schema,
        entities,
        pagination,
        url_prefix: WIDGET_URL_PREFIX.to_string(),
    }))
}

/// GET /forge/{schema}/entities/_table — HTMX table pagination fragment.
pub async fn entity_table_fragment(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<impl IntoResponse, WidgetError> {
    // Delegate to entity_list — same response for widgets
    entity_list(
        State(state),
        OptionalAuth(auth),
        Path(name),
        Query(params),
    )
    .await
}

/// GET /forge/{schema}/entities/new — Create entity form fragment.
pub async fn entity_create_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(WidgetError::from)?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(FieldView::from_definition)
        .collect();
    let schema = SchemaView::from_definition(&schema_def);

    Ok(HtmlTemplate::fragment(WidgetEntityFormTemplate {
        schema,
        fields,
        entity_id: None,
        errors: vec![],
        url_prefix: WIDGET_URL_PREFIX.to_string(),
    }))
}

/// POST /forge/{schema}/entities — Create entity from form.
pub async fn entity_create(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path(name): Path<String>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| WidgetError::schema_not_found(&name))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = schema_forge_backend::entity::Entity::new(schema_name, fields);
            let mut created = state
                .backend
                .create(&entity)
                .await
                .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

            // Apply field filtering
            filter_entity_fields(&mut created, &schema_def, auth.as_ref(), FieldFilterDirection::Read);

            let ref_display =
                resolve_ref_display(&state, &schema_def, std::slice::from_ref(&created)).await;
            let entity_view =
                EntityView::from_entity_with_refs(&created, &schema_def, &ref_display);
            let schema = SchemaView::from_definition(&schema_def);

            Ok(HtmlTemplate::fragment(WidgetEntityDetailTemplate {
                schema,
                entity: entity_view,
                url_prefix: WIDGET_URL_PREFIX.to_string(),
            })
            .into_response())
        }
        Err(errors) => {
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);

            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                HtmlTemplate::fragment(WidgetEntityFormTemplate {
                    schema,
                    fields,
                    entity_id: None,
                    errors,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                }),
            )
                .into_response())
        }
    }
}

/// GET /forge/{schema}/entities/{id} — Entity detail fragment.
pub async fn entity_detail(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| WidgetError::entity_not_found(&name, &id))?;

    let mut entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    filter_entity_fields(&mut entity, &schema_def, auth.as_ref(), FieldFilterDirection::Read);

    let ref_display =
        resolve_ref_display(&state, &schema_def, std::slice::from_ref(&entity)).await;
    let entity_view = EntityView::from_entity_with_refs(&entity, &schema_def, &ref_display);
    let schema = SchemaView::from_definition(&schema_def);

    Ok(HtmlTemplate::fragment(WidgetEntityDetailTemplate {
        schema,
        entity: entity_view,
        url_prefix: WIDGET_URL_PREFIX.to_string(),
    }))
}

/// GET /forge/{schema}/entities/{id}/edit — Edit entity form fragment.
pub async fn entity_edit_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| WidgetError::entity_not_found(&name, &id))?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(|f| {
            let value = entity.field(f.name.as_str());
            FieldView::from_definition_with_value(f, value)
        })
        .collect();
    let schema = SchemaView::from_definition(&schema_def);

    Ok(HtmlTemplate::fragment(WidgetEntityFormTemplate {
        schema,
        fields,
        entity_id: Some(id),
        errors: vec![],
        url_prefix: WIDGET_URL_PREFIX.to_string(),
    }))
}

/// PUT /forge/{schema}/entities/{id} — Update entity from form.
pub async fn entity_update(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| WidgetError::entity_not_found(&name, &id))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity =
                schema_forge_backend::entity::Entity::with_id(entity_id, schema_name, fields);
            let updated = state
                .backend
                .update(&entity)
                .await
                .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

            let ref_display =
                resolve_ref_display(&state, &schema_def, std::slice::from_ref(&updated)).await;
            let entity_view =
                EntityView::from_entity_with_refs(&updated, &schema_def, &ref_display);
            let schema = SchemaView::from_definition(&schema_def);

            Ok(HtmlTemplate::fragment(WidgetEntityDetailTemplate {
                schema,
                entity: entity_view,
                url_prefix: WIDGET_URL_PREFIX.to_string(),
            })
            .into_response())
        }
        Err(errors) => {
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);

            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                HtmlTemplate::fragment(WidgetEntityFormTemplate {
                    schema,
                    fields,
                    entity_id: Some(id),
                    errors,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                }),
            )
                .into_response())
        }
    }
}

/// DELETE /forge/{schema}/entities/{id} — Delete entity.
pub async fn entity_delete(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Delete)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| WidgetError::entity_not_found(&name, &id))?;

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    // Return empty body — HTMX will remove the target element
    Ok(StatusCode::OK)
}

/// GET /forge/{schema}/relation-options/{field} — Relation options for select fields.
pub async fn relation_options(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((target, _field)): Path<(String, String)>,
) -> Result<impl IntoResponse, WidgetError> {
    let schema_def = state
        .registry
        .get(&target)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&target))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(WidgetError::from)?;

    let query = schema_forge_core::query::Query::new(schema_def.id.clone()).with_limit(100);
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    // Find display field
    let display_field = schema_def.annotations.iter().find_map(|a| match a {
        schema_forge_core::types::Annotation::Display { field } => {
            Some(field.as_str().to_string())
        }
        _ => None,
    });

    let mut html = String::from("<option value=\"\">-- Select --</option>\n");
    for entity in &result.entities {
        let id = entity.id.as_str().to_string();
        let label = if let Some(ref df) = display_field {
            entity
                .field(df)
                .map(|v| match v {
                    schema_forge_core::types::DynamicValue::Text(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| id.clone())
        } else {
            schema_def
                .fields
                .iter()
                .find_map(|f| {
                    if matches!(f.field_type, schema_forge_core::types::FieldType::Text(_)) {
                        entity.field(f.name.as_str()).map(|v| match v {
                            schema_forge_core::types::DynamicValue::Text(s) => s.clone(),
                            other => other.to_string(),
                        })
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| id.clone())
        };
        html.push_str(&format!(
            "<option value=\"{}\">{}</option>\n",
            html_escape(&id),
            html_escape(&label),
        ));
    }

    Ok(axum::response::Html(html))
}

/// Basic HTML escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
