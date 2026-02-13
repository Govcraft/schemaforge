use axum::routing::{get, patch, post};
use axum::Router;

use crate::state::ForgeState;

use super::{auth, handlers};

/// Routes that require authentication.
fn cloud_protected_routes() -> Router<ForgeState> {
    Router::new()
        // Dashboard
        .route("/", get(handlers::dashboard))
        // Entity CRUD
        .route(
            "/{schema}/entities",
            get(handlers::entity_list).post(handlers::entity_create),
        )
        .route(
            "/{schema}/entities/_table",
            get(handlers::entity_table_fragment),
        )
        .route(
            "/{schema}/entities/new",
            get(handlers::entity_create_form),
        )
        .route(
            "/{schema}/entities/{id}",
            get(handlers::entity_detail).delete(handlers::entity_delete),
        )
        .route(
            "/{schema}/entities/{id}/edit",
            get(handlers::entity_edit_form).put(handlers::entity_update),
        )
        // Kanban card move
        .route(
            "/{schema}/entities/{id}/move",
            patch(handlers::entity_move),
        )
        // Relation options (shared with widget)
        .route(
            "/{schema}/relation-options/{field}",
            get(handlers::relation_options),
        )
}

/// Public routes that don't require authentication.
fn cloud_public_routes() -> Router<ForgeState> {
    Router::new()
        .route("/theme.css", get(handlers::theme_css))
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout))
}

/// Cloud UI route tree, mounted under `/app/`.
///
/// Protected routes are wrapped with `require_cloud_auth` middleware that
/// redirects unauthenticated requests to `/app/login` and injects
/// `AuthContext` into request extensions for role-based access control.
pub fn cloud_routes() -> Router<ForgeState> {
    let protected = cloud_protected_routes()
        .route_layer(axum::middleware::from_fn(auth::require_cloud_auth));

    protected.merge(cloud_public_routes())
}
