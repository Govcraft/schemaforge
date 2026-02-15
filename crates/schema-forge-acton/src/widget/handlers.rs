use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use schema_forge_core::types::{EntityId, SchemaName};

use crate::access::{
    check_schema_access, filter_entity_fields, AccessAction, FieldFilterDirection, OptionalAuth,
};
use crate::form::form_to_entity_fields;
use crate::shared::resolve_ref_display;
use crate::state::ForgeState;
use crate::template_engine::{render_fragment, render_template_with_status};
use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

use super::error::WidgetError;
use super::templates::{
    WidgetEntityDetailFullTemplate, WidgetEntityFormTemplate, WidgetEntityListTableTemplate,
};

// ===========================================================================
// Widget (bare HTMX fragment) handlers
// ===========================================================================

/// URL prefix for widget routes: `/forge`.
const WIDGET_URL_PREFIX: &str = "/forge";

/// Query params for entity list pagination.
#[derive(Debug, serde::Deserialize, Default)]
pub struct PaginationParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /forge/{schema}/entities -- Paginated entity list.
///
/// Returns HTML table fragment by default. When `Accept: application/json`
/// is sent, returns JSON with schema, entities, and pagination.
pub async fn entity_list(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Response, WidgetError> {
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

    if wants_json(&headers) {
        return Ok(Json(serde_json::json!({
            "schema": schema,
            "entities": entities,
            "pagination": pagination,
        }))
        .into_response());
    }

    Ok(render_fragment(
        &state.template_engine,
        "forge/entity_list.html",
        &WidgetEntityListTableTemplate {
            schema,
            entities,
            pagination,
            url_prefix: WIDGET_URL_PREFIX.to_string(),
        },
    ))
}

/// GET /forge/{schema}/entities/_table -- HTMX table pagination fragment.
pub async fn entity_table_fragment(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Response, WidgetError> {
    // Delegate to entity_list -- same response for widgets
    entity_list(State(state), OptionalAuth(auth), headers, Path(name), Query(params)).await
}

/// GET /forge/{schema}/entities/new -- Create entity form fragment.
///
/// When `Accept: application/json`, returns the schema and field definitions
/// needed to build a create form.
pub async fn entity_create_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Response, WidgetError> {
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

    if wants_json(&headers) {
        return Ok(Json(serde_json::json!({
            "schema": schema,
            "fields": fields,
        }))
        .into_response());
    }

    Ok(render_fragment(
        &state.template_engine,
        "forge/entity_form.html",
        &WidgetEntityFormTemplate {
            schema,
            fields,
            entity_id: None,
            errors: vec![],
            url_prefix: WIDGET_URL_PREFIX.to_string(),
        },
    ))
}

/// POST /forge/{schema}/entities -- Create entity from form.
///
/// When `Accept: application/json`, returns the created entity as JSON
/// or a 422 with `{ "errors": [...] }`.
pub async fn entity_create(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
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

    let schema_name = SchemaName::new(&name).map_err(|_| WidgetError::schema_not_found(&name))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = schema_forge_backend::entity::Entity::new(schema_name, fields);
            let mut created = state
                .backend
                .create(&entity)
                .await
                .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

            // Apply field filtering
            filter_entity_fields(
                &mut created,
                &schema_def,
                auth.as_ref(),
                FieldFilterDirection::Read,
            );

            let ref_display =
                resolve_ref_display(&state, &schema_def, std::slice::from_ref(&created)).await;
            let entity_view =
                EntityView::from_entity_with_refs(&created, &schema_def, &ref_display);
            let schema = SchemaView::from_definition(&schema_def);

            if wants_json(&headers) {
                return Ok(Json(serde_json::json!({
                    "schema": schema,
                    "entity": entity_view,
                }))
                .into_response());
            }

            Ok(render_fragment(
                &state.template_engine,
                "organisms/entity_detail.html",
                &WidgetEntityDetailFullTemplate {
                    schema,
                    entity: entity_view,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                },
            ))
        }
        Err(errors) => {
            if wants_json(&headers) {
                return Ok((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({ "errors": errors })),
                )
                    .into_response());
            }

            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);

            Ok(render_template_with_status(
                &state.template_engine,
                "forge/entity_form.html",
                &WidgetEntityFormTemplate {
                    schema,
                    fields,
                    entity_id: None,
                    errors,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ))
        }
    }
}

/// GET /forge/{schema}/entities/{id} -- Entity detail.
///
/// When `Accept: application/json`, returns `{ "schema": ..., "entity": ... }`.
pub async fn entity_detail(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name).map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id).map_err(|_| WidgetError::entity_not_found(&name, &id))?;

    let mut entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    filter_entity_fields(
        &mut entity,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Read,
    );

    let ref_display = resolve_ref_display(&state, &schema_def, std::slice::from_ref(&entity)).await;
    let entity_view = EntityView::from_entity_with_refs(&entity, &schema_def, &ref_display);
    let schema = SchemaView::from_definition(&schema_def);

    if wants_json(&headers) {
        return Ok(Json(serde_json::json!({
            "schema": schema,
            "entity": entity_view,
        }))
        .into_response());
    }

    Ok(render_fragment(
        &state.template_engine,
        "organisms/entity_detail.html",
        &WidgetEntityDetailFullTemplate {
            schema,
            entity: entity_view,
            url_prefix: WIDGET_URL_PREFIX.to_string(),
        },
    ))
}

/// GET /forge/{schema}/entities/{id}/edit -- Edit entity form.
///
/// When `Accept: application/json`, returns the schema, fields, and entity_id
/// needed to build an edit form.
pub async fn entity_edit_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, WidgetError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| WidgetError::schema_not_found(&name))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(WidgetError::from)?;

    let schema_name = SchemaName::new(&name).map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id).map_err(|_| WidgetError::entity_not_found(&name, &id))?;

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

    if wants_json(&headers) {
        return Ok(Json(serde_json::json!({
            "schema": schema,
            "fields": fields,
            "entity_id": id,
        }))
        .into_response());
    }

    Ok(render_fragment(
        &state.template_engine,
        "forge/entity_form.html",
        &WidgetEntityFormTemplate {
            schema,
            fields,
            entity_id: Some(id),
            errors: vec![],
            url_prefix: WIDGET_URL_PREFIX.to_string(),
        },
    ))
}

/// PUT /forge/{schema}/entities/{id} -- Update entity from form.
///
/// When `Accept: application/json`, returns the updated entity as JSON
/// or a 422 with `{ "errors": [...] }`.
pub async fn entity_update(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
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

    let schema_name = SchemaName::new(&name).map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id).map_err(|_| WidgetError::entity_not_found(&name, &id))?;

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

            if wants_json(&headers) {
                return Ok(Json(serde_json::json!({
                    "schema": schema,
                    "entity": entity_view,
                }))
                .into_response());
            }

            Ok(render_fragment(
                &state.template_engine,
                "organisms/entity_detail.html",
                &WidgetEntityDetailFullTemplate {
                    schema,
                    entity: entity_view,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                },
            ))
        }
        Err(errors) => {
            if wants_json(&headers) {
                return Ok((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({ "errors": errors })),
                )
                    .into_response());
            }

            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);

            Ok(render_template_with_status(
                &state.template_engine,
                "forge/entity_form.html",
                &WidgetEntityFormTemplate {
                    schema,
                    fields,
                    entity_id: Some(id),
                    errors,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ))
        }
    }
}

/// DELETE /forge/{schema}/entities/{id} -- Delete entity.
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

    let schema_name = SchemaName::new(&name).map_err(|_| WidgetError::schema_not_found(&name))?;
    let entity_id = EntityId::parse(&id).map_err(|_| WidgetError::entity_not_found(&name, &id))?;

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(|e| WidgetError::from(crate::error::ForgeError::from(e)))?;

    // Return empty body -- HTMX will remove the target element
    Ok(StatusCode::OK)
}

/// GET /forge/{schema}/relation-options/{field} -- Relation options.
///
/// When `Accept: application/json`, returns `[{ "value": "...", "label": "..." }, ...]`.
pub async fn relation_options(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    headers: HeaderMap,
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
        schema_forge_core::types::Annotation::Display { field } => Some(field.as_str().to_string()),
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
            (id, label)
        })
        .collect();

    if wants_json(&headers) {
        let json_options: Vec<serde_json::Value> = options
            .iter()
            .map(|(value, label)| {
                serde_json::json!({ "value": value, "label": label })
            })
            .collect();
        return Ok(Json(json_options).into_response());
    }

    let mut html = String::from("<option value=\"\">-- Select --</option>\n");
    for (id, label) in &options {
        html.push_str(&format!(
            "<option value=\"{}\">{}</option>\n",
            html_escape(id),
            html_escape(label),
        ));
    }

    Ok(Html(html).into_response())
}

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Check if the client prefers JSON responses via the Accept header.
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("application/json"))
        .unwrap_or(false)
}

/// Basic HTML escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
