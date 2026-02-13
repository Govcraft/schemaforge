use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::state::ForgeState;

use super::handlers;

/// Build the widget route tree.
///
/// All routes are nested under `/{schema}` and serve bare HTMX fragments.
///
/// Route structure:
/// ```text
/// GET  /{schema}/entities           → entity table fragment
/// GET  /{schema}/entities/_table    → pagination fragment (HTMX)
/// GET  /{schema}/entities/new       → create form fragment
/// POST /{schema}/entities           → create entity
/// GET  /{schema}/entities/{id}      → entity detail fragment
/// GET  /{schema}/entities/{id}/edit → edit form fragment
/// PUT  /{schema}/entities/{id}      → update entity
/// DELETE /{schema}/entities/{id}    → delete entity
/// GET  /{schema}/relation-options/{field} → relation select options
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
