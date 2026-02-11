use axum::routing::get;
use axum::Router;

use crate::state::ForgeState;

use super::handlers;

/// Build the admin UI router.
///
/// All routes expect `ForgeState` — the caller applies `.with_state()`.
///
/// The dashboard is mounted at both `/` and the empty path so that
/// `/admin` and `/admin/` both resolve when nested via `nest("/admin", ...)`.
pub fn admin_routes() -> Router<ForgeState> {
    Router::new()
        .route("/", get(handlers::dashboard))
        .route("/schemas/{name}", get(handlers::schema_detail))
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
