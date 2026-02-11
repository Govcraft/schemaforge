use acton_service::service_builder::ServiceBuilder;
use acton_service::versioning::{ApiVersion, VersionedApiBuilder};
use schema_forge_acton::SchemaForgeExtension;
use schema_forge_core::migration::DiffEngine;

use crate::cli::{GlobalOpts, ServeArgs};
use crate::commands::parse::parse_all_schemas;
use crate::config::{load_config, resolve_db_params};
use crate::error::CliError;
use crate::output::OutputContext;

/// Run the `serve` command: start the SchemaForge HTTP server.
///
/// Loads configuration, parses schemas, connects to SurrealDB,
/// builds versioned routes via acton-service, and serves until Ctrl+C.
pub async fn run(
    args: ServeArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    // 1. Load config and resolve DB params
    let config = load_config(global.config.as_deref())?;
    let db_params = resolve_db_params(&config, global);

    // 2. Parse schemas from the schema directory
    output.status("Parsing schemas...");
    let schemas = match parse_all_schemas(std::slice::from_ref(&args.schema_dir)) {
        Ok(s) => {
            output.status(&format!("  {} schemas parsed.", s.len()));
            s
        }
        Err(CliError::NoSchemaFiles { .. }) => {
            output.warn("No schema files found; starting with empty registry.");
            Vec::new()
        }
        Err(e) => return Err(e),
    };

    // 3. Connect to SurrealDB (try remote, fall back to in-memory)
    let backend = super::connect_backend(&db_params, output).await?;

    // 4. Build the SchemaForge extension
    let extension = SchemaForgeExtension::builder()
        .with_backend(backend)
        .build()
        .await
        .map_err(|e| CliError::Server {
            message: format!("failed to build SchemaForge extension: {e}"),
        })?;

    // 5. Apply parsed schemas
    if !schemas.is_empty() {
        output.status("Applying schemas...");
        let backend_ref = extension.state().backend.clone();
        for schema in &schemas {
            let existing = backend_ref
                .load_schema_metadata(&schema.name)
                .await
                .map_err(CliError::Backend)?;

            let plan = if let Some(old) = existing {
                DiffEngine::diff(&old, schema)
            } else {
                DiffEngine::create_new(schema)
            };

            if !plan.is_empty() {
                backend_ref
                    .apply_migration(&schema.name, &plan.steps)
                    .await
                    .map_err(CliError::Backend)?;
                backend_ref
                    .store_schema_metadata(schema)
                    .await
                    .map_err(CliError::Backend)?;

                // Update registry
                extension
                    .registry()
                    .insert(schema.name.as_str().to_string(), schema.clone())
                    .await;

                output.status(&format!("  Applied {}", schema.name.as_str()));
            }
        }
    }

    // 6. Warn about --watch
    if args.watch {
        output.warn("--watch is not yet implemented; schemas will not auto-reload.");
    }

    // 7. Build versioned routes via acton-service
    #[cfg(feature = "admin-ui")]
    let routes = build_versioned_routes_with_admin(&extension);
    #[cfg(not(feature = "admin-ui"))]
    let routes = build_versioned_routes(&extension);

    // 8. Configure and serve via acton-service
    let mut svc_config = acton_service::config::Config::<()>::default();
    svc_config.service.port = args.port;
    svc_config.service.name = "schema-forge".to_string();

    let bind_addr = format!("{}:{}", args.host, args.port);
    output.success(&format!(
        "SchemaForge server listening on http://{bind_addr}"
    ));
    output.status("  Routes:");
    output.status("    GET  /health");
    output.status("    GET  /ready");
    output.status("    POST /api/v1/forge/schemas");
    output.status("    GET  /api/v1/forge/schemas");
    output.status("    GET  /api/v1/forge/schemas/:name");
    output.status("    PUT  /api/v1/forge/schemas/:name");
    output.status("    DEL  /api/v1/forge/schemas/:name");
    output.status("    POST /api/v1/forge/schemas/:schema/entities");
    output.status("    GET  /api/v1/forge/schemas/:schema/entities");
    output.status("    GET  /api/v1/forge/schemas/:schema/entities/:id");
    output.status("    PUT  /api/v1/forge/schemas/:schema/entities/:id");
    output.status("    DEL  /api/v1/forge/schemas/:schema/entities/:id");
    #[cfg(feature = "admin-ui")]
    output.status("    GET  /admin/");
    output.status("  Press Ctrl+C to stop.");

    let service = ServiceBuilder::new()
        .with_config(svc_config)
        .with_routes(routes)
        .build();

    service.serve().await.map_err(|e| CliError::Server {
        message: format!("server error: {e}"),
    })?;

    output.success("Server shut down gracefully.");
    Ok(())
}

/// Build versioned routes using acton-service's VersionedApiBuilder.
///
/// Nests SchemaForge's routes under `/api/v1/forge/`.
#[cfg(not(feature = "admin-ui"))]
fn build_versioned_routes(
    extension: &SchemaForgeExtension,
) -> acton_service::service_builder::VersionedRoutes {
    VersionedApiBuilder::new()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, |router| {
            extension.register_versioned_routes(router)
        })
        .build_routes()
}

/// Build versioned routes with admin UI frontend routes.
#[cfg(feature = "admin-ui")]
fn build_versioned_routes_with_admin(
    extension: &SchemaForgeExtension,
) -> acton_service::service_builder::VersionedRoutes {
    VersionedApiBuilder::new()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, |router| {
            extension.register_versioned_routes(router)
        })
        .with_frontend_routes(|router| extension.register_admin_routes(router))
        .build_routes()
}

/// Build a test router using `register_routes()` directly (no acton-service layer).
#[cfg(test)]
fn build_test_router(extension: &SchemaForgeExtension) -> axum::Router {
    extension.register_routes(axum::Router::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use schema_forge_surrealdb::SurrealBackend;
    use tower::ServiceExt;

    /// Build a test router backed by an in-memory SurrealDB.
    async fn test_router() -> axum::Router {
        let backend = SurrealBackend::connect_memory("test", "serve_test")
            .await
            .expect("in-memory backend");
        let extension = SchemaForgeExtension::builder()
            .with_backend(backend)
            .build()
            .await
            .expect("extension");
        build_test_router(&extension)
    }

    #[tokio::test]
    async fn forge_schemas_endpoint_returns_empty_list() {
        let router = test_router().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/forge/schemas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // list_schemas returns { "schemas": [...], "count": N }
        assert_eq!(json["count"], 0);
        assert!(json["schemas"].is_array());
        assert_eq!(json["schemas"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn build_versioned_routes_is_callable() {
        // Compile-time verification that build_versioned_routes has the right signature
        let _: fn(&SchemaForgeExtension) -> acton_service::service_builder::VersionedRoutes =
            build_versioned_routes;
    }
}
