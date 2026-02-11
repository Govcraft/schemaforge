use axum::routing::{get, post};
use axum::Router;

use crate::state::ForgeState;

use super::{auth, handlers};

/// Routes that require authentication.
///
/// **Route ordering**: `/schemas/new` and `/schemas/_*` are registered before
/// `/schemas/{name}` to avoid being captured as a name parameter.
pub fn protected_routes() -> Router<ForgeState> {
    Router::new()
        .route("/", get(handlers::dashboard))
        // Schema editor: static paths first
        .route("/schemas/new", get(handlers::schema_create_form))
        .route("/schemas", post(handlers::schema_create))
        .route("/schemas/_preview", post(handlers::schema_preview))
        .route(
            "/schemas/_field-row/{index}",
            get(handlers::field_row_fragment),
        )
        .route(
            "/schemas/_type-constraints/{field_type}",
            get(handlers::type_constraints_fragment),
        )
        // Schema detail/edit/delete: dynamic {name} path
        .route(
            "/schemas/{name}",
            get(handlers::schema_detail)
                .post(handlers::schema_update)
                .delete(handlers::schema_delete),
        )
        .route("/schemas/{name}/edit", get(handlers::schema_edit_form))
        // Entity routes
        .route(
            "/schemas/{name}/entities",
            get(handlers::entity_list).post(handlers::entity_create),
        )
        .route(
            "/schemas/{name}/entities/_table",
            get(handlers::entity_table_fragment),
        )
        .route(
            "/schemas/{name}/entities/new",
            get(handlers::entity_create_form),
        )
        .route(
            "/schemas/{name}/entities/{id}",
            get(handlers::entity_detail)
                .put(handlers::entity_update)
                .delete(handlers::entity_delete),
        )
        .route(
            "/schemas/{name}/entities/{id}/edit",
            get(handlers::entity_edit_form),
        )
        .route(
            "/schemas/{name}/relation-options/{field}",
            get(handlers::relation_options),
        )
}

/// Public routes that don't require authentication.
pub fn public_routes() -> Router<ForgeState> {
    Router::new()
        .route("/static/admin.css", get(handlers::admin_css))
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout))
}

/// Build the admin UI router with authentication middleware.
///
/// Protected routes are wrapped with `require_auth` middleware that redirects
/// unauthenticated requests to `/admin/login`. Public routes (login, logout,
/// static assets) are accessible without auth.
///
/// A session layer must be applied externally (by `register_admin_routes`).
pub fn admin_routes() -> Router<ForgeState> {
    let protected = protected_routes()
        .route_layer(axum::middleware::from_fn(auth::require_auth));

    protected.merge(public_routes())
}
