use acton_service::prelude::{AuthSession, TypedSession};
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use schema_forge_backend::auth::AuthContext;
use schema_forge_core::types::{Annotation, EntityId, FieldType, SchemaName};

use crate::access::{
    check_schema_access, filter_entity_fields, AccessAction, FieldFilterDirection, OptionalAuth,
};
use crate::form::form_to_entity_fields;
use crate::shared::resolve_ref_display;
use crate::state::ForgeState;
use crate::theme::{DetailStyle, ListStyle, Theme};
use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

use schema_forge_core::query::{AggregateOp, AggregateQuery, FieldPath};

use super::auth::CloudUserView;
use super::css;
use super::templates::{
    CloudDashboardTemplate, CloudEntityDetailTemplate, CloudEntityFormTemplate,
    CloudEntityListBodyTemplate, CloudEntityListKanbanTemplate, CloudEntityListTemplate,
    DashboardCard, NavSchemaEntry,
};

// ---------------------------------------------------------------------------
// Theme slot fields
// ---------------------------------------------------------------------------

/// Sanitized theme slot fields for template rendering.
struct ThemeSlots {
    favicon_url: Option<String>,
    head_html: Option<String>,
    nav_extra_html: Option<String>,
    footer_html: Option<String>,
}

/// Extract and sanitize theme slot fields.
fn theme_slots(theme: &Theme) -> ThemeSlots {
    ThemeSlots {
        favicon_url: theme.favicon_url.clone(),
        head_html: theme.head_html.as_ref().map(|h| css::sanitize_html(h)),
        nav_extra_html: theme.nav_extra_html.as_ref().map(|h| css::sanitize_html(h)),
        footer_html: theme.footer_html.as_ref().map(|h| css::sanitize_html(h)),
    }
}

// ---------------------------------------------------------------------------
// MiniJinja render helper
// ---------------------------------------------------------------------------

/// Render a cloud template via MiniJinja.
pub(crate) fn render_cloud<T: serde::Serialize>(state: &ForgeState, name: &str, ctx: &T) -> Response {
    match state.template_engine.render(name, ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {e}")).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Navigation helpers
// ---------------------------------------------------------------------------

/// Build navigation entries from registered schemas, respecting theme ordering
/// and role-based access control.
async fn build_nav(
    state: &ForgeState,
    theme: &Theme,
    auth: Option<&AuthContext>,
) -> Vec<NavSchemaEntry> {
    let all = state.registry.list().await;
    let mut entries: Vec<NavSchemaEntry> = all
        .iter()
        .filter(|s| {
            // Hide system schemas from nav
            !s.annotations.iter().any(|a| matches!(a, Annotation::System))
        })
        .filter(|s| {
            // Filter by role-based access control
            check_schema_access(s, auth, AccessAction::Read).is_ok()
        })
        .map(|s| {
            let name = s.name.as_str().to_string();
            NavSchemaEntry {
                label: theme.schema_label(&name),
                url_name: name,
            }
        })
        .collect();

    // If dashboard_schemas is configured, put those first
    if !theme.dashboard_schemas.is_empty() {
        entries.sort_by_key(|e| {
            theme
                .dashboard_schemas
                .iter()
                .position(|s| s == &e.url_name)
                .unwrap_or(usize::MAX)
        });
    }

    entries
}

/// String name for nav style.
fn nav_style_name(theme: &Theme) -> &str {
    match theme.nav_style {
        crate::theme::NavStyle::Sidebar => "sidebar",
        crate::theme::NavStyle::TopNav => "topnav",
        crate::theme::NavStyle::Minimal => "minimal",
    }
}

/// String name for list style.
fn list_style_name(style: &ListStyle) -> &str {
    match style {
        ListStyle::Table => "table",
        ListStyle::Cards => "cards",
        ListStyle::Compact => "compact",
        ListStyle::Kanban => "kanban",
    }
}

/// String name for detail style.
fn detail_style_name(style: &DetailStyle) -> &str {
    match style {
        DetailStyle::Full => "full",
        DetailStyle::Split => "split",
        DetailStyle::Tabbed => "tabbed",
    }
}

/// GET /app/ — Dashboard.
pub async fn dashboard(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let theme = state.theme.load();
    let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

    // Build schema cards with aggregate widgets
    let all_schemas = state.registry.list().await;
    let mut schema_cards = Vec::new();

    // If dashboard_schemas configured, use those; otherwise show non-system schemas
    let schemas_to_show: Vec<_> = if !theme.dashboard_schemas.is_empty() {
        theme
            .dashboard_schemas
            .iter()
            .filter_map(|name| all_schemas.iter().find(|s| s.name.as_str() == name))
            .collect()
    } else {
        all_schemas
            .iter()
            .filter(|s| !s.annotations.iter().any(|a| matches!(a, Annotation::System)))
            .collect()
    };

    // Filter schemas by access control
    let schemas_to_show: Vec<_> = schemas_to_show
        .into_iter()
        .filter(|s| check_schema_access(s, auth.as_ref(), AccessAction::Read).is_ok())
        .collect();

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
            state.backend.aggregate(&agg_query).await.unwrap_or_default()
        };

        let name = schema.name.as_str().to_string();
        let label = theme.schema_label(&name);
        for r in &results {
            let format_hint = match &r.op {
                AggregateOp::Sum { field } | AggregateOp::Avg { field } => {
                    schema
                        .field(field.root())
                        .and_then(|f| f.format_hint())
                }
                _ => None,
            };
            schema_cards.push(DashboardCard {
                url_name: name.clone(),
                label: label.clone(),
                widget_label: widget_label(&r.op),
                display_value: format_widget_value(&r.op, r.value, format_hint),
            });
        }
    }

    let slots = theme_slots(&theme);
    Ok(render_cloud(
        &state,
        "cloud/dashboard.html",
        &CloudDashboardTemplate {
            app_title: theme.app_title(),
            nav_style: nav_style_name(&theme).to_string(),
            logo_url: theme.logo_url.clone(),
            nav_schemas,
            active_nav: "dashboard".to_string(),
            schema_cards,
            current_user,
            favicon_url: slots.favicon_url,
            head_html: slots.head_html,
            nav_extra_html: slots.nav_extra_html,
            footer_html: slots.footer_html,
        },
    ))
}

/// GET /app/theme.css — Serve generated CSS.
pub async fn theme_css(State(state): State<ForgeState>) -> impl IntoResponse {
    let theme = state.theme.load();
    let css = css::generate_css(&theme);
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        css,
    )
}

/// Query params for entity list pagination and filtering.
#[derive(Debug, Default)]
pub struct ListParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub filters: std::collections::HashMap<String, String>,
}

/// Extract list params from a raw query string HashMap.
fn parse_list_params(raw: &std::collections::HashMap<String, String>) -> ListParams {
    let limit = raw.get("limit").and_then(|v| v.parse().ok());
    let offset = raw.get("offset").and_then(|v| v.parse().ok());
    let filters: std::collections::HashMap<String, String> = raw
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
fn build_filter(
    filters: &std::collections::HashMap<String, String>,
) -> Option<schema_forge_core::query::Filter> {
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

/// GET /app/{schema}/entities — Entity list page.
pub async fn entity_list(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path(name): Path<String>,
    Query(raw_params): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let params = parse_list_params(&raw_params);
    let theme = state.theme.load();
    let list_style = theme.resolve_list_style(&name);

    // Check if kanban should be used (explicit style OR @dashboard(layout: "kanban"))
    let kanban_field = crate::views::find_kanban_field(&schema_def);
    let use_kanban = matches!(list_style, ListStyle::Kanban) || {
        kanban_field.is_some()
            && schema_def.annotations.iter().any(|a| matches!(
                a,
                Annotation::Dashboard { layout: Some(l), .. } if l == "kanban"
            ))
    };

    if use_kanban {
        if let Some((field_name, variants)) = kanban_field {
            // Kanban: fetch up to 500, no pagination
            let mut query = schema_forge_core::query::Query::new(schema_def.id.clone())
                .with_limit(500);
            if let Some(filter) = build_filter(&params.filters) {
                query = query.with_filter(filter);
            }
            let result = state
                .backend
                .query(&query)
                .await
                .map_err(|e| CloudError::Internal(e.to_string()))?;

            let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
            let entities: Vec<EntityView> = result
                .entities
                .iter()
                .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
                .collect();

            let columns =
                crate::views::group_entities_by_field(entities, &field_name, &variants);
            let mut schema = SchemaView::from_definition(&schema_def);
            schema.apply_theme_labels(&theme);
            let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

            let slots = theme_slots(&theme);
            return Ok(render_cloud(
                &state,
                "cloud/entity_list_kanban.html",
                &CloudEntityListKanbanTemplate {
                    app_title: theme.app_title(),
                    nav_style: nav_style_name(&theme).to_string(),
                    logo_url: theme.logo_url.clone(),
                    nav_schemas,
                    active_nav: name,
                    schema,
                    columns,
                    kanban_field: field_name,
                    current_user,
                    favicon_url: slots.favicon_url,
                    head_html: slots.head_html,
                    nav_extra_html: slots.nav_extra_html,
                    footer_html: slots.footer_html,
                },
            ));
        }
    }

    // Standard list (table/cards/compact)
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
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let mut schema = SchemaView::from_definition(&schema_def);

    schema.apply_theme_labels(&theme);
    let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

    let filter_fields = crate::views::extract_filter_fields(&schema_def, &params.filters);

    let slots = theme_slots(&theme);
    Ok(render_cloud(
        &state,
        "cloud/entity_list.html",
        &CloudEntityListTemplate {
            app_title: theme.app_title(),
            nav_style: nav_style_name(&theme).to_string(),
            logo_url: theme.logo_url.clone(),
            nav_schemas,
            active_nav: name.clone(),
            schema,
            entities,
            pagination,
            list_style: list_style_name(list_style).to_string(),
            filter_fields,
            current_user,
            favicon_url: slots.favicon_url,
            head_html: slots.head_html,
            nav_extra_html: slots.nav_extra_html,
            footer_html: slots.footer_html,
        },
    ))
}

/// GET /app/{schema}/entities/_table — HTMX pagination fragment.
pub async fn entity_table_fragment(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path(name): Path<String>,
    Query(raw_params): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, CloudError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

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
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let mut schema = SchemaView::from_definition(&schema_def);

    let theme = state.theme.load();
    schema.apply_theme_labels(&theme);
    let list_style = theme.resolve_list_style(&name);

    let filter_fields = crate::views::extract_filter_fields(&schema_def, &params.filters);

    Ok(render_cloud(
        &state,
        "cloud/fragments/entity_list_body.html",
        &CloudEntityListBodyTemplate {
            schema,
            entities,
            pagination,
            list_style: list_style_name(list_style).to_string(),
            filter_fields,
        },
    ))
}

/// GET /app/{schema}/entities/new — Create form.
pub async fn entity_create_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path(name): Path<String>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(FieldView::from_definition)
        .collect();
    let mut schema = SchemaView::from_definition(&schema_def);

    let theme = state.theme.load();
    schema.apply_theme_labels(&theme);
    let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

    let slots = theme_slots(&theme);
    Ok(render_cloud(
        &state,
        "cloud/entity_form.html",
        &CloudEntityFormTemplate {
            app_title: theme.app_title(),
            nav_style: nav_style_name(&theme).to_string(),
            logo_url: theme.logo_url.clone(),
            nav_schemas,
            active_nav: name,
            schema,
            fields,
            entity_id: None,
            errors: vec![],
            current_user,
            favicon_url: slots.favicon_url,
            head_html: slots.head_html,
            nav_extra_html: slots.nav_extra_html,
            footer_html: slots.footer_html,
        },
    ))
}

/// POST /app/{schema}/entities — Create entity.
pub async fn entity_create(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path(name): Path<String>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let schema_name =
        SchemaName::new(&name).map_err(|_| CloudError::NotFound(format!("Invalid schema: {name}")))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = schema_forge_backend::entity::Entity::new(schema_name, fields);
            let created = state
                .backend
                .create(&entity)
                .await
                .map_err(|e| CloudError::Internal(e.to_string()))?;

            if name == "Theme" {
                crate::theme::reload_theme(&state).await;
            }

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
            let mut schema = SchemaView::from_definition(&schema_def);

            let theme = state.theme.load();
            schema.apply_theme_labels(&theme);
            let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

            let slots = theme_slots(&theme);
            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                render_cloud(
                    &state,
                    "cloud/entity_form.html",
                    &CloudEntityFormTemplate {
                        app_title: theme.app_title(),
                        nav_style: nav_style_name(&theme).to_string(),
                        logo_url: theme.logo_url.clone(),
                        nav_schemas,
                        active_nav: name,
                        schema,
                        fields,
                        entity_id: None,
                        errors,
                        current_user,
                        favicon_url: slots.favicon_url,
                        head_html: slots.head_html,
                        nav_extra_html: slots.nav_extra_html,
                        footer_html: slots.footer_html,
                    },
                ),
            )
                .into_response())
        }
    }
}

/// GET /app/{schema}/entities/{id} — Entity detail page.
pub async fn entity_detail(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let schema_name =
        SchemaName::new(&name).map_err(|_| CloudError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| CloudError::NotFound(format!("Entity '{id}' not found")))?;

    let mut entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    filter_entity_fields(
        &mut entity,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Read,
    );

    let ref_display = resolve_ref_display(&state, &schema_def, std::slice::from_ref(&entity)).await;
    let entity_view = EntityView::from_entity_with_refs(&entity, &schema_def, &ref_display);
    let mut schema = SchemaView::from_definition(&schema_def);

    let theme = state.theme.load();
    schema.apply_theme_labels(&theme);
    let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;
    let detail_style = theme.resolve_detail_style(&name);

    let slots = theme_slots(&theme);
    Ok(render_cloud(
        &state,
        "cloud/entity_detail.html",
        &CloudEntityDetailTemplate {
            app_title: theme.app_title(),
            nav_style: nav_style_name(&theme).to_string(),
            logo_url: theme.logo_url.clone(),
            nav_schemas,
            active_nav: name,
            schema,
            entity: entity_view,
            detail_style: detail_style_name(detail_style).to_string(),
            current_user,
            favicon_url: slots.favicon_url,
            head_html: slots.head_html,
            nav_extra_html: slots.nav_extra_html,
            footer_html: slots.footer_html,
        },
    ))
}

/// GET /app/{schema}/entities/{id}/edit — Edit form.
pub async fn entity_edit_form(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let schema_name =
        SchemaName::new(&name).map_err(|_| CloudError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| CloudError::NotFound(format!("Entity '{id}' not found")))?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(|f| {
            let value = entity.field(f.name.as_str());
            FieldView::from_definition_with_value(f, value)
        })
        .collect();
    let mut schema = SchemaView::from_definition(&schema_def);

    let theme = state.theme.load();
    schema.apply_theme_labels(&theme);
    let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

    let slots = theme_slots(&theme);
    Ok(render_cloud(
        &state,
        "cloud/entity_form.html",
        &CloudEntityFormTemplate {
            app_title: theme.app_title(),
            nav_style: nav_style_name(&theme).to_string(),
            logo_url: theme.logo_url.clone(),
            nav_schemas,
            active_nav: name,
            schema,
            fields,
            entity_id: Some(id),
            errors: vec![],
            current_user,
            favicon_url: slots.favicon_url,
            head_html: slots.head_html,
            nav_extra_html: slots.nav_extra_html,
            footer_html: slots.footer_html,
        },
    ))
}

/// PUT /app/{schema}/entities/{id} — Update entity.
pub async fn entity_update(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    session: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, CloudError> {
    let current_user = CloudUserView::from_session(session.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let schema_name =
        SchemaName::new(&name).map_err(|_| CloudError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| CloudError::NotFound(format!("Entity '{id}' not found")))?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity =
                schema_forge_backend::entity::Entity::with_id(entity_id, schema_name, fields);
            state
                .backend
                .update(&entity)
                .await
                .map_err(|e| CloudError::Internal(e.to_string()))?;

            if name == "Theme" {
                crate::theme::reload_theme(&state).await;
            }

            Ok(axum::response::Redirect::to(&format!(
                "/app/{}/entities/{}",
                name, id
            ))
            .into_response())
        }
        Err(errors) => {
            let fields: Vec<FieldView> = schema_def
                .fields
                .iter()
                .map(FieldView::from_definition)
                .collect();
            let mut schema = SchemaView::from_definition(&schema_def);

            let theme = state.theme.load();
            schema.apply_theme_labels(&theme);
            let nav_schemas = build_nav(&state, &theme, auth.as_ref()).await;

            let slots = theme_slots(&theme);
            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                render_cloud(
                    &state,
                    "cloud/entity_form.html",
                    &CloudEntityFormTemplate {
                        app_title: theme.app_title(),
                        nav_style: nav_style_name(&theme).to_string(),
                        logo_url: theme.logo_url.clone(),
                        nav_schemas,
                        active_nav: name,
                        schema,
                        fields,
                        entity_id: Some(id),
                        errors,
                        current_user,
                        favicon_url: slots.favicon_url,
                        head_html: slots.head_html,
                        nav_extra_html: slots.nav_extra_html,
                        footer_html: slots.footer_html,
                    },
                ),
            )
                .into_response())
        }
    }
}

/// DELETE /app/{schema}/entities/{id} — Delete entity.
pub async fn entity_delete(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, CloudError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Delete)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let schema_name =
        SchemaName::new(&name).map_err(|_| CloudError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| CloudError::NotFound(format!("Entity '{id}' not found")))?;

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    if name == "Theme" {
        crate::theme::reload_theme(&state).await;
    }

    Ok(StatusCode::OK)
}

/// PATCH /app/{schema}/entities/{id}/move — Move entity (kanban card drag).
///
/// Expects form data: `field=<field_name>&value=<new_value>`
pub async fn entity_move(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<impl IntoResponse, CloudError> {
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", name)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let schema_name =
        SchemaName::new(&name).map_err(|_| CloudError::NotFound(format!("Invalid schema: {name}")))?;
    let entity_id = EntityId::parse(&id)
        .map_err(|_| CloudError::NotFound(format!("Entity '{id}' not found")))?;

    // Extract field and value from form data
    let field_name = form_data
        .iter()
        .find(|(k, _)| k == "field")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| CloudError::Internal("Missing 'field' parameter".to_string()))?;
    let new_value = form_data
        .iter()
        .find(|(k, _)| k == "value")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| CloudError::Internal("Missing 'value' parameter".to_string()))?;

    // Fetch existing entity
    let existing = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    // Merge the single field update
    let mut fields = existing.fields.clone();
    // Determine the right DynamicValue type — for kanban moves it's typically an enum
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
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    Ok(StatusCode::OK)
}

/// GET /app/{schema}/relation-options/{field} — Relation options for select fields.
pub async fn relation_options(
    State(state): State<ForgeState>,
    OptionalAuth(auth): OptionalAuth,
    Path((target, _field)): Path<(String, String)>,
) -> Result<impl IntoResponse, CloudError> {
    let schema_def = state
        .registry
        .get(&target)
        .await
        .ok_or_else(|| CloudError::NotFound(format!("Schema '{}' not found", target)))?;

    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)
        .map_err(|e| CloudError::Forbidden(e.to_string()))?;

    let query = schema_forge_core::query::Query::new(schema_def.id.clone()).with_limit(100);
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| CloudError::Internal(e.to_string()))?;

    let display_field = schema_def.annotations.iter().find_map(|a| match a {
        Annotation::Display { field } => Some(field.as_str().to_string()),
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
            id.clone()
        };
        html.push_str(&format!(
            "<option value=\"{}\">{}</option>\n",
            html_escape(&id),
            html_escape(&label),
        ));
    }

    Ok(axum::response::Html(html))
}

/// Parse a widget spec string like "count", "sum:value", "avg:price" into an `AggregateOp`.
fn parse_widget_spec(spec: &str) -> Option<AggregateOp> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("count") {
        return Some(AggregateOp::Count);
    }
    if let Some(field) = spec.strip_prefix("sum:") {
        return FieldPath::parse(field.trim()).ok().map(|field| AggregateOp::Sum { field });
    }
    if let Some(field) = spec.strip_prefix("avg:") {
        return FieldPath::parse(field.trim()).ok().map(|field| AggregateOp::Avg { field });
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Cloud UI error type.
pub enum CloudError {
    NotFound(String),
    Forbidden(String),
    Internal(String),
}

impl IntoResponse for CloudError {
    fn into_response(self) -> Response {
        match self {
            CloudError::NotFound(msg) => {
                (StatusCode::NOT_FOUND, format!("Not found: {msg}")).into_response()
            }
            CloudError::Forbidden(msg) => {
                let html = format!(
                    r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Access Denied</title>
    <link rel="stylesheet" href="/app/theme.css">
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
                (StatusCode::FORBIDDEN, Html(html)).into_response()
            }
            CloudError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Internal error: {msg}"),
            )
                .into_response(),
        }
    }
}

impl From<crate::error::ForgeError> for CloudError {
    fn from(e: crate::error::ForgeError) -> Self {
        CloudError::Internal(e.to_string())
    }
}
