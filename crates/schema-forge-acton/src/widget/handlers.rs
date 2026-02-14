use std::collections::HashMap;

use acton_service::prelude::{AuthSession, TypedSession};
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use schema_forge_backend::auth::AuthContext;
use schema_forge_core::query::{AggregateOp, AggregateQuery, FieldPath};
use schema_forge_core::types::{Annotation, EntityId, FieldType, SchemaName};

use crate::access::{
    check_schema_access, filter_entity_fields, AccessAction, FieldFilterDirection, OptionalAuth,
};
use crate::form::form_to_entity_fields;
use crate::shared::resolve_ref_display;
use crate::state::ForgeState;
use crate::template_engine::{render_fragment, render_template_with_status};
use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

use super::auth::SiteUserView;
use super::error::WidgetError;
use super::templates::{
    BreadcrumbItem, DashboardCard, HeadingAction, NavSchemaEntry, SiteDashboardTemplate,
    SiteEntityDetailTemplate, SiteEntityFormTemplate, SiteEntityListBodyTemplate,
    SiteEntityListKanbanTemplate, SiteEntityListTemplate, StatItem, WidgetEntityDetailFullTemplate,
    WidgetEntityFormTemplate, WidgetEntityListTableTemplate,
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

/// GET /forge/{schema}/entities -- Paginated entity table fragment.
pub async fn entity_list(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
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

    Ok(render_fragment(
        &state.template_engine,
        "forge/entity_list_table.html",
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
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Response, WidgetError> {
    // Delegate to entity_list -- same response for widgets
    entity_list(State(state), OptionalAuth(auth), Path(name), Query(params)).await
}

/// GET /forge/{schema}/entities/new -- Create entity form fragment.
pub async fn entity_create_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
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

            Ok(render_fragment(
                &state.template_engine,
                "organisms/entity_detail_full.html",
                &WidgetEntityDetailFullTemplate {
                    schema,
                    entity: entity_view,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                },
            ))
        }
        Err(errors) => {
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

/// GET /forge/{schema}/entities/{id} -- Entity detail fragment.
pub async fn entity_detail(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
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

    Ok(render_fragment(
        &state.template_engine,
        "organisms/entity_detail_full.html",
        &WidgetEntityDetailFullTemplate {
            schema,
            entity: entity_view,
            url_prefix: WIDGET_URL_PREFIX.to_string(),
        },
    ))
}

/// GET /forge/{schema}/entities/{id}/edit -- Edit entity form fragment.
pub async fn entity_edit_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
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

            Ok(render_fragment(
                &state.template_engine,
                "organisms/entity_detail_full.html",
                &WidgetEntityDetailFullTemplate {
                    schema,
                    entity: entity_view,
                    url_prefix: WIDGET_URL_PREFIX.to_string(),
                },
            ))
        }
        Err(errors) => {
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

/// GET /forge/{schema}/relation-options/{field} -- Relation options for select fields.
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
        schema_forge_core::types::Annotation::Display { field } => Some(field.as_str().to_string()),
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

    Ok(Html(html))
}

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Basic HTML escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ===========================================================================
// Site (full-page) handlers
// ===========================================================================

// ---------------------------------------------------------------------------
// MiniJinja render helper
// ---------------------------------------------------------------------------

/// Render a site template via MiniJinja.
pub(crate) fn render_site<T: serde::Serialize>(
    state: &ForgeState,
    name: &str,
    ctx: &T,
) -> Response {
    match state.template_engine.render(name, ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {e}"),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Navigation helpers
// ---------------------------------------------------------------------------

/// Build navigation entries from registered schemas, respecting role-based
/// access control.
async fn build_nav(state: &ForgeState, auth: Option<&AuthContext>) -> Vec<NavSchemaEntry> {
    let all = state.registry.list().await;
    let mut entries: Vec<NavSchemaEntry> = all
        .iter()
        .filter(|s| {
            !s.annotations
                .iter()
                .any(|a| matches!(a, Annotation::System))
        })
        .filter(|s| check_schema_access(s, auth, AccessAction::Read).is_ok())
        .map(|s| {
            let name = s.name.as_str().to_string();
            NavSchemaEntry {
                label: name.clone(),
                url_name: name,
                entity_count: None,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.label.cmp(&b.label));
    entries
}

// ---------------------------------------------------------------------------
// List param parsing and filtering
// ---------------------------------------------------------------------------

/// Query params for entity list pagination and filtering.
#[derive(Debug, Default)]
pub struct ListParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub filters: HashMap<String, String>,
}

/// Extract list params from a raw query string HashMap.
fn parse_list_params(raw: &HashMap<String, String>) -> ListParams {
    let limit = raw.get("limit").and_then(|v| v.parse().ok());
    let offset = raw.get("offset").and_then(|v| v.parse().ok());
    let filters: HashMap<String, String> = raw
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("filter_")
                .filter(|_| !v.is_empty())
                .map(|field| (field.to_string(), v.clone()))
        })
        .collect();
    ListParams {
        limit,
        offset,
        filters,
    }
}

/// Build a backend Filter from active filter params.
fn build_filter(filters: &HashMap<String, String>) -> Option<schema_forge_core::query::Filter> {
    let parts: Vec<schema_forge_core::query::Filter> = filters
        .iter()
        .map(|(field, value)| {
            schema_forge_core::query::Filter::eq(
                FieldPath::single(field),
                schema_forge_core::types::DynamicValue::Text(value.clone()),
            )
        })
        .collect();
    match parts.len() {
        0 => None,
        1 => Some(parts.into_iter().next().unwrap()),
        _ => Some(schema_forge_core::query::Filter::and(parts)),
    }
}

// ---------------------------------------------------------------------------
// Dashboard widget helpers
// ---------------------------------------------------------------------------

/// Parse a widget spec string like "count", "sum:value", "avg:price" into an `AggregateOp`.
fn parse_widget_spec(spec: &str) -> Option<AggregateOp> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("count") {
        return Some(AggregateOp::Count);
    }
    if let Some(field) = spec.strip_prefix("sum:") {
        return FieldPath::parse(field.trim())
            .ok()
            .map(|field| AggregateOp::Sum { field });
    }
    if let Some(field) = spec.strip_prefix("avg:") {
        return FieldPath::parse(field.trim())
            .ok()
            .map(|field| AggregateOp::Avg { field });
    }
    None
}

/// Human-readable label for an aggregate operation.
fn widget_label(op: &AggregateOp) -> String {
    match op {
        AggregateOp::Count => "Count".to_string(),
        AggregateOp::Sum { field } => format!("Total {}", capitalize(field.leaf())),
        AggregateOp::Avg { field } => format!("Avg {}", capitalize(field.leaf())),
        _ => "Value".to_string(),
    }
}

/// Format a widget value for display.
fn format_widget_value(op: &AggregateOp, value: f64, format_hint: Option<&str>) -> String {
    match op {
        AggregateOp::Count => format!("{}", value as u64),
        _ => {
            if let Some(hint) = format_hint {
                let (fmt_type, symbol) = match hint.split_once(':') {
                    Some((t, s)) => (t, s),
                    None => (hint, ""),
                };
                match fmt_type {
                    "currency" => format!(
                        "{}{}",
                        symbol,
                        crate::views::format_number_with_commas(value, 2)
                    ),
                    "percent" => {
                        format!("{}%", crate::views::format_number_with_commas(value, 1))
                    }
                    _ => format!("{value:.2}"),
                }
            } else {
                format!("{value:.2}")
            }
        }
    }
}

/// Capitalize the first letter of a string.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

// ---------------------------------------------------------------------------
// Site handlers (full-page with nav)
// ---------------------------------------------------------------------------

/// GET /app/ -- Dashboard.
pub async fn dashboard(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let nav_schemas = build_nav(&state, auth.as_ref()).await;

    // Build schema cards with aggregate widgets
    let all_schemas = state.registry.list().await;

    let schemas_to_show: Vec<_> = all_schemas
        .iter()
        .filter(|s| {
            !s.annotations
                .iter()
                .any(|a| matches!(a, Annotation::System))
        })
        .collect();

    // Filter schemas by access control
    let schemas_to_show: Vec<_> = schemas_to_show
        .into_iter()
        .filter(|s| check_schema_access(s, auth.as_ref(), AccessAction::Read).is_ok())
        .collect();

    let mut schema_cards = Vec::new();

    for schema in schemas_to_show {
        // Extract @dashboard widget specs, default to ["count"]
        let widget_specs = schema
            .annotations
            .iter()
            .find_map(|a| match a {
                Annotation::Dashboard { widgets, .. } if !widgets.is_empty() => {
                    Some(widgets.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| vec!["count".to_string()]);

        let ops: Vec<AggregateOp> = widget_specs
            .iter()
            .filter_map(|spec| parse_widget_spec(spec))
            .collect();

        let results = if ops.is_empty() {
            vec![]
        } else {
            let agg_query = AggregateQuery::new(schema.id.clone()).with_ops(ops);
            state
                .backend
                .aggregate(&agg_query)
                .await
                .unwrap_or_default()
        };

        let name = schema.name.as_str().to_string();
        for r in &results {
            let format_hint = match &r.op {
                AggregateOp::Sum { field } | AggregateOp::Avg { field } => {
                    schema.field(field.root()).and_then(|f| f.format_hint())
                }
                _ => None,
            };
            schema_cards.push(DashboardCard {
                url_name: name.clone(),
                label: name.clone(),
                widget_label: widget_label(&r.op),
                display_value: format_widget_value(&r.op, r.value, format_hint),
            });
        }
    }

    // Build stat items from schema cards for the stats section
    let stats: Vec<StatItem> = schema_cards
        .iter()
        .map(|card| StatItem {
            label: format!("{} {}", card.label, card.widget_label),
            value: card.display_value.clone(),
            unit: None,
            trend_value: None,
            trend_direction: None,
            previous_value: None,
            icon_svg: None,
            link_url: Some(format!("/app/{}/entities", card.url_name)),
            link_label: Some(format!("View all {}", card.label)),
        })
        .collect();

    Ok(render_site(
        &state,
        "cloud/dashboard.html",
        &SiteDashboardTemplate {
            nav_schemas,
            active_nav: "dashboard".to_string(),
            schema_cards,
            current_user,
            heading_actions: vec![],
            breadcrumbs: vec![],
            stats,
        },
    ))
}

/// GET /app/{schema}/entities -- Entity list page.
pub async fn site_entity_list(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path(name): Path<String>,
    Query(raw_params): Query<HashMap<String, String>>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let params = parse_list_params(&raw_params);

    // Check if kanban should be used via @dashboard(layout: "kanban") annotation
    let kanban_field = crate::views::find_kanban_field(&schema_def);
    let use_kanban = {
        kanban_field.is_some()
            && schema_def.annotations.iter().any(|a| {
                matches!(
                    a,
                    Annotation::Dashboard { layout: Some(l), .. } if l == "kanban"
                )
            })
    };

    if use_kanban {
        if let Some((field_name, variants)) = kanban_field {
            // Kanban: fetch up to 500, no pagination
            let mut query =
                schema_forge_core::query::Query::new(schema_def.id.clone()).with_limit(500);
            if let Some(filter) = build_filter(&params.filters) {
                query = query.with_filter(filter);
            }
            let result = state
                .backend
                .query(&query)
                .await
                .map_err(|e| SiteError::Internal(e.to_string()))?;

            let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
            let entities: Vec<EntityView> = result
                .entities
                .iter()
                .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
                .collect();

            let columns = crate::views::group_entities_by_field(entities, &field_name, &variants);
            let schema = SchemaView::from_definition(&schema_def);
            let nav_schemas = build_nav(&state, auth.as_ref()).await;

            return Ok(render_site(
                &state,
                "cloud/entity_list_kanban.html",
                &SiteEntityListKanbanTemplate {
                    nav_schemas,
                    active_nav: name.clone(),
                    schema,
                    columns,
                    kanban_field: field_name,
                    current_user,
                    heading_actions: vec![HeadingAction {
                        url: format!("/app/{}/entities/new", name),
                        label: "Create New".to_string(),
                        class: "sf-btn-primary".to_string(),
                    }],
                    breadcrumbs: vec![
                        BreadcrumbItem {
                            label: "Dashboard".to_string(),
                            url: Some("/app/".to_string()),
                        },
                        BreadcrumbItem {
                            label: name.clone(),
                            url: None,
                        },
                    ],
                },
            ));
        }
    }

    // Standard list (table)
    let limit = params.limit.unwrap_or(25);
    let offset = params.offset.unwrap_or(0);

    let mut query = schema_forge_core::query::Query::new(schema_def.id.clone())
        .with_limit(limit)
        .with_offset(offset);
    if let Some(filter) = build_filter(&params.filters) {
        query = query.with_filter(filter);
    }
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let schema = SchemaView::from_definition(&schema_def);
    let nav_schemas = build_nav(&state, auth.as_ref()).await;

    let filter_fields = crate::views::extract_filter_fields(&schema_def, &params.filters);

    Ok(render_site(
        &state,
        "cloud/entity_list.html",
        &SiteEntityListTemplate {
            nav_schemas,
            active_nav: name.clone(),
            schema,
            entities,
            pagination,
            list_style: "table".to_string(),
            filter_fields,
            current_user,
            heading_actions: vec![HeadingAction {
                url: format!("/app/{}/entities/new", name),
                label: "Create New".to_string(),
                class: "sf-btn-primary".to_string(),
            }],
            breadcrumbs: vec![
                BreadcrumbItem {
                    label: "Dashboard".to_string(),
                    url: Some("/app/".to_string()),
                },
                BreadcrumbItem {
                    label: name.clone(),
                    url: None,
                },
            ],
        },
    ))
}

/// GET /app/{schema}/entities/_table -- HTMX pagination fragment.
pub async fn site_entity_table_fragment(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path(name): Path<String>,
    Query(raw_params): Query<HashMap<String, String>>,
) -> Result<Response, SiteError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let params = parse_list_params(&raw_params);
    let limit = params.limit.unwrap_or(25);
    let offset = params.offset.unwrap_or(0);

    let mut query = schema_forge_core::query::Query::new(schema_def.id.clone())
        .with_limit(limit)
        .with_offset(offset);
    if let Some(filter) = build_filter(&params.filters) {
        query = query.with_filter(filter);
    }
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let schema = SchemaView::from_definition(&schema_def);

    let filter_fields = crate::views::extract_filter_fields(&schema_def, &params.filters);

    Ok(render_site(
        &state,
        "cloud/fragments/entity_list_body.html",
        &SiteEntityListBodyTemplate {
            schema,
            entities,
            pagination,
            list_style: "table".to_string(),
            filter_fields,
        },
    ))
}

/// GET /app/{schema}/entities/new -- Create form.
pub async fn site_entity_create_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path(name): Path<String>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(FieldView::from_definition)
        .collect();
    let schema = SchemaView::from_definition(&schema_def);
    let nav_schemas = build_nav(&state, auth.as_ref()).await;

    Ok(render_site(
        &state,
        "cloud/entity_form.html",
        &SiteEntityFormTemplate {
            nav_schemas,
            active_nav: name.clone(),
            schema,
            fields,
            entity_id: None,
            errors: vec![],
            current_user,
            heading_actions: vec![],
            breadcrumbs: vec![
                BreadcrumbItem {
                    label: "Dashboard".to_string(),
                    url: Some("/app/".to_string()),
                },
                BreadcrumbItem {
                    label: name.clone(),
                    url: Some(format!("/app/{}/entities", name)),
                },
                BreadcrumbItem {
                    label: "New".to_string(),
                    url: None,
                },
            ],
        },
    ))
}

/// POST /app/{schema}/entities -- Create entity.
pub async fn site_entity_create(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path(name): Path<String>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| SiteError::NotFound(format!("Invalid schema: {name}")))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = schema_forge_backend::entity::Entity::new(schema_name, fields);
            let created = state
                .backend
                .create(&entity)
                .await
                .map_err(|e| SiteError::Internal(e.to_string()))?;

            Ok(axum::response::Redirect::to(&format!(
                "/app/{}/entities/{}",
                name,
                created.id.as_str()
            ))
            .into_response())
        }
        Err(errors) => {
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);
            let nav_schemas = build_nav(&state, auth.as_ref()).await;

            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                render_site(
                    &state,
                    "cloud/entity_form.html",
                    &SiteEntityFormTemplate {
                        nav_schemas,
                        active_nav: name.clone(),
                        schema,
                        fields,
                        entity_id: None,
                        errors,
                        current_user,
                        heading_actions: vec![],
                        breadcrumbs: vec![
                            BreadcrumbItem {
                                label: "Dashboard".to_string(),
                                url: Some("/app/".to_string()),
                            },
                            BreadcrumbItem {
                                label: name.clone(),
                                url: Some(format!("/app/{}/entities", name)),
                            },
                            BreadcrumbItem {
                                label: "New".to_string(),
                                url: None,
                            },
                        ],
                    },
                ),
            )
                .into_response())
        }
    }
}

/// GET /app/{schema}/entities/{id} -- Entity detail page.
pub async fn site_entity_detail(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| SiteError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| SiteError::NotFound(format!("Entity '{id}' not found")))?;

    let mut entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    filter_entity_fields(
        &mut entity,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Read,
    );

    let ref_display = resolve_ref_display(&state, &schema_def, std::slice::from_ref(&entity)).await;
    let entity_view = EntityView::from_entity_with_refs(&entity, &schema_def, &ref_display);
    let schema = SchemaView::from_definition(&schema_def);
    let nav_schemas = build_nav(&state, auth.as_ref()).await;

    let display_val = entity_view.display_value.clone();
    Ok(render_site(
        &state,
        "cloud/entity_detail.html",
        &SiteEntityDetailTemplate {
            nav_schemas,
            active_nav: name.clone(),
            schema,
            entity: entity_view,
            detail_style: "full".to_string(),
            current_user,
            heading_actions: vec![HeadingAction {
                url: format!("/app/{}/entities/{}/edit", name, id),
                label: "Edit".to_string(),
                class: "sf-btn-primary sf-btn-sm".to_string(),
            }],
            breadcrumbs: vec![
                BreadcrumbItem {
                    label: "Dashboard".to_string(),
                    url: Some("/app/".to_string()),
                },
                BreadcrumbItem {
                    label: name.clone(),
                    url: Some(format!("/app/{}/entities", name)),
                },
                BreadcrumbItem {
                    label: display_val,
                    url: None,
                },
            ],
        },
    ))
}

/// GET /app/{schema}/entities/{id}/edit -- Edit form.
pub async fn site_entity_edit_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| SiteError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| SiteError::NotFound(format!("Entity '{id}' not found")))?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(|f| {
            let value = entity.field(f.name.as_str());
            FieldView::from_definition_with_value(f, value)
        })
        .collect();
    let schema = SchemaView::from_definition(&schema_def);
    let nav_schemas = build_nav(&state, auth.as_ref()).await;

    Ok(render_site(
        &state,
        "cloud/entity_form.html",
        &SiteEntityFormTemplate {
            nav_schemas,
            active_nav: name.clone(),
            schema,
            fields,
            entity_id: Some(id.clone()),
            errors: vec![],
            current_user,
            heading_actions: vec![],
            breadcrumbs: vec![
                BreadcrumbItem {
                    label: "Dashboard".to_string(),
                    url: Some("/app/".to_string()),
                },
                BreadcrumbItem {
                    label: name.clone(),
                    url: Some(format!("/app/{}/entities", name)),
                },
                BreadcrumbItem {
                    label: format!("Edit {}", id),
                    url: None,
                },
            ],
        },
    ))
}

/// PUT /app/{schema}/entities/{id} -- Update entity.
pub async fn site_entity_update(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, SiteError> {
    let current_user = SiteUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| SiteError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| SiteError::NotFound(format!("Entity '{id}' not found")))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity =
                schema_forge_backend::entity::Entity::with_id(entity_id, schema_name, fields);
            state
                .backend
                .update(&entity)
                .await
                .map_err(|e| SiteError::Internal(e.to_string()))?;

            Ok(
                axum::response::Redirect::to(&format!("/app/{}/entities/{}", name, id))
                    .into_response(),
            )
        }
        Err(errors) => {
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let schema = SchemaView::from_definition(&schema_def);
            let nav_schemas = build_nav(&state, auth.as_ref()).await;

            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                render_site(
                    &state,
                    "cloud/entity_form.html",
                    &SiteEntityFormTemplate {
                        nav_schemas,
                        active_nav: name.clone(),
                        schema,
                        fields,
                        entity_id: Some(id.clone()),
                        errors,
                        current_user,
                        heading_actions: vec![],
                        breadcrumbs: vec![
                            BreadcrumbItem {
                                label: "Dashboard".to_string(),
                                url: Some("/app/".to_string()),
                            },
                            BreadcrumbItem {
                                label: name.clone(),
                                url: Some(format!("/app/{}/entities", name)),
                            },
                            BreadcrumbItem {
                                label: format!("Edit {}", id),
                                url: None,
                            },
                        ],
                    },
                ),
            )
                .into_response())
        }
    }
}

/// DELETE /app/{schema}/entities/{id} -- Delete entity.
pub async fn site_entity_delete(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, SiteError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Delete)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| SiteError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| SiteError::NotFound(format!("Entity '{id}' not found")))?;

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    Ok(StatusCode::OK)
}

/// PATCH /app/{schema}/entities/{id}/move -- Move entity (kanban card drag).
///
/// Expects form data: `field=<field_name>&value=<new_value>`
pub async fn site_entity_move(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<impl IntoResponse, SiteError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let schema_name = SchemaName::new(&name)
        .map_err(|_| SiteError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| SiteError::NotFound(format!("Entity '{id}' not found")))?;

    // Extract field and value from form data
    let field_name = form_data
        .iter()
        .find(|(k, _)| k == "field")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| SiteError::Internal("Missing 'field' parameter".to_string()))?;
    let new_value = form_data
        .iter()
        .find(|(k, _)| k == "value")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| SiteError::Internal("Missing 'value' parameter".to_string()))?;

    // Fetch existing entity
    let existing = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    // Merge the single field update
    let mut fields = existing.fields.clone();
    // Determine the right DynamicValue type -- for kanban moves it is typically an enum
    let dv = if let Some(fd) = schema_def.field(&field_name) {
        if matches!(fd.field_type, FieldType::Enum(_)) {
            schema_forge_core::types::DynamicValue::Enum(new_value)
        } else {
            schema_forge_core::types::DynamicValue::Text(new_value)
        }
    } else {
        schema_forge_core::types::DynamicValue::Text(new_value)
    };
    fields.insert(field_name, dv);

    let entity = schema_forge_backend::entity::Entity::with_id(entity_id, schema_name, fields);
    state
        .backend
        .update(&entity)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    Ok(StatusCode::OK)
}

/// GET /app/{schema}/relation-options/{field} -- Relation options for select fields.
pub async fn site_relation_options(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((target, _field)): Path<(String, String)>,
) -> Result<impl IntoResponse, SiteError> {
    let schema_def = state
        .registry
        .get(&target)
        .await
        .ok_or_else(|| SiteError::NotFound(format!("Schema '{}' not found", target)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| SiteError::Forbidden(e.to_string()))?;

    let query = schema_forge_core::query::Query::new(schema_def.id.clone()).with_limit(100);
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| SiteError::Internal(e.to_string()))?;

    let display_field = schema_def.annotations.iter().find_map(|a| match a {
        Annotation::Display { field } => Some(field.as_str().to_string()),
        _ => None,
    });

    let mut html_out = String::from("<option value=\"\">-- Select --</option>\n");
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
            id.clone()
        };
        html_out.push_str(&format!(
            "<option value=\"{}\">{}</option>\n",
            html_escape(&id),
            html_escape(&label),
        ));
    }

    Ok(Html(html_out))
}

// ---------------------------------------------------------------------------
// Site error type
// ---------------------------------------------------------------------------

/// Site UI error type.
pub enum SiteError {
    NotFound(String),
    Forbidden(String),
    Internal(String),
}

impl IntoResponse for SiteError {
    fn into_response(self) -> Response {
        match self {
            SiteError::NotFound(msg) => {
                (StatusCode::NOT_FOUND, format!("Not found: {msg}")).into_response()
            }
            SiteError::Forbidden(msg) => {
                let html_content = format!(
                    r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Access Denied</title>
</head>
<body class="sf-app">
    <div class="sf-error-page">
        <div class="sf-error-code">403</div>
        <div class="sf-error-message">{}</div>
        <a href="/app/" class="sf-btn sf-btn-primary">Back to Dashboard</a>
    </div>
</body>
</html>"#,
                    html_escape(&msg),
                );
                (StatusCode::FORBIDDEN, Html(html_content)).into_response()
            }
            SiteError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Internal error: {msg}"),
            )
                .into_response(),
        }
    }
}

impl From<crate::error::ForgeError> for SiteError {
    fn from(e: crate::error::ForgeError) -> Self {
        SiteError::Internal(e.to_string())
    }
}
