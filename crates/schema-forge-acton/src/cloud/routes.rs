use axum::routing::get;
use axum::Router;

use crate::state::ForgeState;

use super::handlers;

/// Cloud UI route tree, mounted under `/app/`.
pub fn cloud_routes() -> Router<ForgeState> {
    Router::new()
        // Dashboard
        .route("/", get(handlers::dashboard))
        // Theme CSS
        .route("/theme.css", get(handlers::theme_css))
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
        // Relation options (shared with widget)
        .route(
            "/{schema}/relation-options/{field}",
            get(handlers::relation_options),
        )
}
