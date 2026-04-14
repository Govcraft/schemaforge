pub mod auth;
pub mod entities;
pub mod query_params;
pub mod schemas;

pub use auth::auth_routes;

use axum::routing::{get, post};
use axum::Router;

use acton_service::state::AppState;

use crate::config::SchemaForgeConfig;

/// Build the SchemaForge router with all schema and entity CRUD routes.
///
/// The router is generic over `AppState<SchemaForgeConfig>`. Handler state
/// comes from the actor extension (`state.actor::<ForgeActor>()`) and from
/// a `ForgeState` extension layer set by the caller.
///
/// Auth middleware is applied externally when the state is available
/// (see [`SchemaForgeExtension::register_routes`]).
pub fn forge_routes() -> Router<AppState<SchemaForgeConfig>> {
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
                .patch(entities::patch_entity)
                .delete(entities::delete_entity),
        )
}
