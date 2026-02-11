pub mod entities;
pub mod query_params;
pub mod schemas;

use axum::routing::{get, post};
use axum::Router;

use crate::state::ForgeState;

/// Build the SchemaForge router with all schema and entity CRUD routes.
///
/// The router is returned without state applied -- the caller (extension.rs)
/// provides the `ForgeState`. Auth middleware is applied externally when the
/// state is available (see [`SchemaForgeExtension::register_routes`]).
pub fn forge_routes() -> Router<ForgeState> {
    Router::new()
        // Schema management
        .route(
            "/schemas",
            post(schemas::create_schema).get(schemas::list_schemas),
        )
        .route(
            "/schemas/{name}",
            get(schemas::get_schema)
                .put(schemas::update_schema)
                .delete(schemas::delete_schema),
        )
        // Entity CRUD (nested under schema)
        .route(
            "/schemas/{schema}/entities",
            post(entities::create_entity).get(entities::list_entities),
        )
        .route(
            "/schemas/{schema}/entities/query",
            post(entities::query_entities),
        )
        .route(
            "/schemas/{schema}/entities/{id}",
            get(entities::get_entity)
                .put(entities::update_entity)
                .delete(entities::delete_entity),
        )
}
