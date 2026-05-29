pub mod auth;
pub mod entities;
pub mod export;
pub mod files;
pub mod health;
pub mod invites;
pub mod meta;
pub mod permissions;
pub mod query_params;
pub mod schemas;
pub mod users;

pub use auth::auth_routes;
pub use health::{
    schema_forge_health_middleware, HealthResponse, SCHEMA_FORGE_ACTON_VERSION,
    SCHEMA_FORGE_SERVICE_NAME,
};
pub use meta::{meta_routes, MetaAuth, MetaBuild, MetaInfo};

use axum::routing::{delete, get, post};
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
        // Bulk export (sync streamable CSV/NDJSON; defers async formats)
        .route(
            "/schemas/{schema}/entities/export",
            post(export::export_entities),
        )
        .route(
            "/schemas/{schema}/entities/{id}",
            get(entities::get_entity)
                .put(entities::update_entity)
                .patch(entities::patch_entity)
                .delete(entities::delete_entity),
        )
        // File fields (presigned upload, confirm, and download)
        .route(
            "/schemas/{schema}/entities/{id}/fields/{field}/upload-url",
            post(files::mint_upload_url),
        )
        .route(
            "/schemas/{schema}/entities/{id}/fields/{field}/confirm-upload",
            post(files::confirm_upload),
        )
        .route(
            "/schemas/{schema}/entities/{id}/fields/{field}",
            get(files::download_file),
        )
        .route(
            "/schemas/{schema}/entities/{id}/fields/{field}/scan-complete",
            post(files::scan_complete),
        )
        // Admin-shell permission summary (drives sidebar gating)
        .route("/permissions", get(permissions::get_permissions))
        // User management
        .route("/users", post(users::create_user).get(users::list_users))
        .route("/users/roles", get(users::list_roles))
        .route(
            "/users/{username}",
            delete(users::delete_user).put(users::update_user),
        )
        .route("/users/{username}/password", post(users::change_password))
}
