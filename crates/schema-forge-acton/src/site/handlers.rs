use axum::extract::{Form, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};


use acton_service::htmx::HxRedirect;
use acton_service::prelude::{AuthSession, TypedSession};
use acton_service::session::{FlashMessage, FlashMessages};
use schema_forge_core::types::{EntityId, SchemaName};

use crate::form::form_to_entity_fields;
use crate::shared::resolve_ref_display;
use crate::state::ForgeState;
use crate::template_engine::{render_template, render_template_with_status};
use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

use super::auth::CurrentUserView;
use super::error::SiteError;
use super::templates::{
    EntityDetailTemplate, EntityFormTemplate, EntityListTemplate, FlashView, HomeTemplate,
    SchemaSummary,
};

/// URL prefix for site entity routes.
const SITE_URL_PREFIX: &str = "/site/schemas";

/// GET /site/static/site.css — Serve site CSS.
pub async fn site_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css")],
        include_str!("../../static/css/site.css"),
    )
}

/// GET /site/ — Home page listing available schemas.
pub async fn home(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    flash: FlashMessages,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let all_schemas = state.registry.list().await;

    let mut schemas: Vec<SchemaSummary> = all_schemas
        .iter()
        .filter(|s| !s.is_system())
        .map(|s| SchemaSummary {
            name: s.name.as_str().to_string(),
            field_count: s.fields.len(),
        })
        .collect();
    schemas.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(render_template(
        &state.template_engine,
        "site/index.html",
        &HomeTemplate {
            schemas,
            current_user,
            flash: FlashView::from_flash_messages(flash.into_messages()),
        },
    ))
}

/// Query params for entity list pagination.
#[derive(Debug, serde::Deserialize, Default)]
pub struct PaginationParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /site/schemas/{name}/entities — Entity list page.
pub async fn entity_list(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    flash: FlashMessages,
    Path(name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::Internal {
            message: format!("Schema '{name}' not found"),
        })?;

    let limit = params.limit.unwrap_or(25);
    let offset = params.offset.unwrap_or(0);

    let query = schema_forge_core::query::Query::new(schema_def.id.clone())
        .with_limit(limit)
        .with_offset(offset);
    let result = state
        .backend
        .query(&query)
        .await
        .map_err(|e| SiteError::Internal {
            message: e.to_string(),
        })?;

    let total_count = result.total_count.unwrap_or(result.entities.len());
    let ref_display = resolve_ref_display(&state, &schema_def, &result.entities).await;
    let entities: Vec<EntityView> = result
        .entities
        .iter()
        .map(|e| EntityView::from_entity_with_refs(e, &schema_def, &ref_display))
        .collect();
    let pagination = PaginationView::new(total_count, limit, offset);
    let schema = SchemaView::from_definition(&schema_def);

    Ok(render_template(
        &state.template_engine,
        "site/entities.html",
        &EntityListTemplate {
            schema,
            entities,
            pagination,
            current_user,
            url_prefix: SITE_URL_PREFIX.to_string(),
            flash: FlashView::from_flash_messages(flash.into_messages()),
        },
    ))
}

/// GET /site/schemas/{name}/entities/_table — HTMX pagination fragment.
pub async fn entity_table_fragment(
    state: State<ForgeState>,
    auth: TypedSession<AuthSession>,
    flash: FlashMessages,
    path: Path<String>,
    query: Query<PaginationParams>,
) -> Result<Response, SiteError> {
    entity_list(state, auth, flash, path, query).await
}

/// GET /site/schemas/{name}/entities/new — Create entity form.
pub async fn entity_create_form(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    flash: FlashMessages,
    Path(name): Path<String>,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::Internal {
            message: format!("Schema '{name}' not found"),
        })?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(FieldView::from_definition)
        .collect();
    let schema = SchemaView::from_definition(&schema_def);

    Ok(render_template(
        &state.template_engine,
        "site/entity_form.html",
        &EntityFormTemplate {
            schema,
            fields,
            entity_id: None,
            errors: vec![],
            current_user,
            url_prefix: SITE_URL_PREFIX.to_string(),
            flash: FlashView::from_flash_messages(flash.into_messages()),
        },
    ))
}

/// POST /site/schemas/{name}/entities — Create entity.
pub async fn entity_create(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    Path(name): Path<String>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::Internal {
            message: format!("Schema '{name}' not found"),
        })?;

    let schema_name = SchemaName::new(&name).map_err(|_| SiteError::Internal {
        message: format!("Invalid schema name '{name}'"),
    })?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity = schema_forge_backend::entity::Entity::new(schema_name, fields);
            let created = state
                .backend
                .create(&entity)
                .await
                .map_err(|e| SiteError::Internal {
                    message: e.to_string(),
                })?;

            let _ = FlashMessages::push(
                auth.session(),
                FlashMessage::success(format!("{name} created successfully")),
            )
            .await;

            let url = format!(
                "{}/{}/entities/{}",
                SITE_URL_PREFIX,
                name,
                created.id.as_str()
            );
            Ok((HxRedirect(url), ()).into_response())
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
                "site/entity_form.html",
                &EntityFormTemplate {
                    schema,
                    fields,
                    entity_id: None,
                    errors,
                    current_user,
                    url_prefix: SITE_URL_PREFIX.to_string(),
                    flash: vec![],
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ))
        }
    }
}

/// GET /site/schemas/{name}/entities/{id} — Entity detail page.
pub async fn entity_detail(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    flash: FlashMessages,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::Internal {
            message: format!("Schema '{name}' not found"),
        })?;

    let schema_name = SchemaName::new(&name).map_err(|_| SiteError::Internal {
        message: format!("Invalid schema name '{name}'"),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| SiteError::Internal {
        message: format!("Invalid entity id '{id}'"),
    })?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal {
            message: e.to_string(),
        })?;

    let ref_display =
        resolve_ref_display(&state, &schema_def, std::slice::from_ref(&entity)).await;
    let entity_view = EntityView::from_entity_with_refs(&entity, &schema_def, &ref_display);
    let schema = SchemaView::from_definition(&schema_def);

    Ok(render_template(
        &state.template_engine,
        "site/entity_detail.html",
        &EntityDetailTemplate {
            schema,
            entity: entity_view,
            current_user,
            url_prefix: SITE_URL_PREFIX.to_string(),
            flash: FlashView::from_flash_messages(flash.into_messages()),
        },
    ))
}

/// GET /site/schemas/{name}/entities/{id}/edit — Edit entity form.
pub async fn entity_edit_form(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    flash: FlashMessages,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::Internal {
            message: format!("Schema '{name}' not found"),
        })?;

    let schema_name = SchemaName::new(&name).map_err(|_| SiteError::Internal {
        message: format!("Invalid schema name '{name}'"),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| SiteError::Internal {
        message: format!("Invalid entity id '{id}'"),
    })?;

    let entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal {
            message: e.to_string(),
        })?;

    let fields: Vec<FieldView> = schema_def
        .fields
        .iter()
        .map(|f| {
            let value = entity.field(f.name.as_str());
            FieldView::from_definition_with_value(f, value)
        })
        .collect();
    let schema = SchemaView::from_definition(&schema_def);

    Ok(render_template(
        &state.template_engine,
        "site/entity_form.html",
        &EntityFormTemplate {
            schema,
            fields,
            entity_id: Some(id),
            errors: vec![],
            current_user,
            url_prefix: SITE_URL_PREFIX.to_string(),
            flash: FlashView::from_flash_messages(flash.into_messages()),
        },
    ))
}

/// PUT /site/schemas/{name}/entities/{id} — Update entity.
pub async fn entity_update(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
    Form(form_data): Form<Vec<(String, String)>>,
) -> Result<Response, SiteError> {
    let current_user = CurrentUserView::from_session(auth.data());
    let schema_def = state
        .registry
        .get(&name)
        .await
        .ok_or_else(|| SiteError::Internal {
            message: format!("Schema '{name}' not found"),
        })?;

    let schema_name = SchemaName::new(&name).map_err(|_| SiteError::Internal {
        message: format!("Invalid schema name '{name}'"),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| SiteError::Internal {
        message: format!("Invalid entity id '{id}'"),
    })?;

    match form_to_entity_fields(&schema_def, &form_data) {
        Ok(fields) => {
            let entity =
                schema_forge_backend::entity::Entity::with_id(entity_id, schema_name, fields);
            state
                .backend
                .update(&entity)
                .await
                .map_err(|e| SiteError::Internal {
                    message: e.to_string(),
                })?;

            let _ = FlashMessages::push(
                auth.session(),
                FlashMessage::success(format!("{name} updated successfully")),
            )
            .await;

            let url = format!("{}/{}/entities/{}", SITE_URL_PREFIX, name, id);
            Ok((HxRedirect(url), ()).into_response())
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
                "site/entity_form.html",
                &EntityFormTemplate {
                    schema,
                    fields,
                    entity_id: Some(id),
                    errors,
                    current_user,
                    url_prefix: SITE_URL_PREFIX.to_string(),
                    flash: vec![],
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ))
        }
    }
}

/// DELETE /site/schemas/{name}/entities/{id} — Delete entity.
///
/// Returns an HX-Redirect header so HTMX navigates to the entity list.
pub async fn entity_delete(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, SiteError> {
    let schema_name = SchemaName::new(&name).map_err(|_| SiteError::Internal {
        message: format!("Invalid schema name '{name}'"),
    })?;
    let entity_id = EntityId::parse(&id).map_err(|_| SiteError::Internal {
        message: format!("Invalid entity id '{id}'"),
    })?;

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(|e| SiteError::Internal {
            message: e.to_string(),
        })?;

    let _ = FlashMessages::push(
        auth.session(),
        FlashMessage::success(format!("{name} deleted")),
    )
    .await;

    let url = format!("{}/{}/entities", SITE_URL_PREFIX, name);
    Ok((HxRedirect(url), ()).into_response())
}
