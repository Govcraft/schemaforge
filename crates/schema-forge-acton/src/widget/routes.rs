use axum::routing::{delete, get, patch, post, put};
use axum::Router;

use crate::state::ForgeState;

use super::{auth, handlers};

/// Build the widget route tree.
///
/// All routes are nested under `/{schema}` and serve bare HTMX fragments.
///
/// Route structure:
/// ```text
/// GET  /{schema}/entities           -> entity table fragment
/// GET  /{schema}/entities/_table    -> pagination fragment (HTMX)
/// GET  /{schema}/entities/new       -> create form fragment
/// POST /{schema}/entities           -> create entity
/// GET  /{schema}/entities/{id}      -> entity detail fragment
/// GET  /{schema}/entities/{id}/edit -> edit form fragment
/// PUT  /{schema}/entities/{id}      -> update entity
/// DELETE /{schema}/entities/{id}    -> delete entity
/// GET  /{schema}/relation-options/{field} -> relation select options
/// ```
pub fn widget_routes() -> Router<ForgeState> {
    Router::new()
        .route("/{schema}/entities", get(handlers::entity_list))
        .route("/{schema}/entities", post(handlers::entity_create))
        .route(
            "/{schema}/entities/_table",
            get(handlers::entity_table_fragment),
        )
        .route("/{schema}/entities/new", get(handlers::entity_create_form))
        .route("/{schema}/entities/{id}", get(handlers::entity_detail))
        .route("/{schema}/entities/{id}", put(handlers::entity_update))
        .route("/{schema}/entities/{id}", delete(handlers::entity_delete))
        .route(
            "/{schema}/entities/{id}/edit",
            get(handlers::entity_edit_form),
        )
        .route(
            "/{schema}/relation-options/{field}",
            get(handlers::relation_options),
        )
}

/// Routes that require authentication (full site pages).
fn site_protected_routes() -> Router<ForgeState> {
    Router::new()
        // Dashboard
        .route("/", get(handlers::dashboard))
        // Entity CRUD
        .route(
            "/{schema}/entities",
            get(handlers::site_entity_list).post(handlers::site_entity_create),
        )
        .route(
            "/{schema}/entities/_table",
            get(handlers::site_entity_table_fragment),
        )
        .route(
            "/{schema}/entities/new",
            get(handlers::site_entity_create_form),
        )
        .route(
            "/{schema}/entities/{id}",
            get(handlers::site_entity_detail).delete(handlers::site_entity_delete),
        )
        .route(
            "/{schema}/entities/{id}/edit",
            get(handlers::site_entity_edit_form).put(handlers::site_entity_update),
        )
        // Kanban card move
        .route(
            "/{schema}/entities/{id}/move",
            patch(handlers::site_entity_move),
        )
        // Relation options
        .route(
            "/{schema}/relation-options/{field}",
            get(handlers::site_relation_options),
        )
}

/// Public routes that don't require authentication.
fn site_public_routes() -> Router<ForgeState> {
    Router::new()
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout))
}

/// Site UI route tree, mounted under `/app/`.
///
/// Protected routes are wrapped with `require_site_auth` middleware that
/// redirects unauthenticated requests to `/app/login` and injects
/// `AuthContext` into request extensions for role-based access control.
pub fn site_routes() -> Router<ForgeState> {
    let protected =
        site_protected_routes().route_layer(axum::middleware::from_fn(auth::require_site_auth));

    protected.merge(site_public_routes())
}
