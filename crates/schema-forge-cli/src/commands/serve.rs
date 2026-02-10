use axum::routing::get;
use axum::Router;
use schema_forge_acton::SchemaForgeExtension;
use schema_forge_core::migration::DiffEngine;
use schema_forge_surrealdb::SurrealBackend;

use crate::cli::{GlobalOpts, ServeArgs};
use crate::commands::parse::parse_all_schemas;
use crate::config::{load_config, resolve_db_params};
use crate::error::CliError;
use crate::output::OutputContext;
use crate::progress;

/// Run the `serve` command: start the SchemaForge HTTP server.
///
/// Loads configuration, parses schemas, connects to SurrealDB,
/// builds the axum router with forge routes and a health endpoint,
/// and serves until Ctrl+C.
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
    let spinner = if output.show_progress() {
        Some(progress::create_spinner("Connecting to backend..."))
    } else {
        None
    };

    let backend =
        match SurrealBackend::connect(&db_params.url, &db_params.namespace, &db_params.database)
            .await
        {
            Ok(b) => {
                if let Some(sp) = &spinner {
                    progress::finish_spinner(
                        sp,
                        &format!("Connected to {}", db_params.url),
                    );
                }
                b
            }
            Err(e) => {
                if let Some(sp) = &spinner {
                    progress::finish_spinner_error(
                        sp,
                        &format!("Remote connection failed: {e}"),
                    );
                }
                output.warn(&format!(
                    "Could not connect to {}; falling back to in-memory backend.",
                    db_params.url
                ));
                SurrealBackend::connect_memory(&db_params.namespace, &db_params.database)
                    .await
                    .map_err(|e| CliError::Server {
                        message: format!("in-memory backend failed: {e}"),
                    })?
            }
        };

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

    // 7. Build the router
    let router = build_router(&extension);

    // 8. Bind and serve
    let bind_addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| CliError::Server {
            message: format!("failed to bind to {bind_addr}: {e}"),
        })?;

    output.success(&format!(
        "SchemaForge server listening on http://{bind_addr}"
    ));
    output.status("  Routes:");
    output.status("    GET  /health");
    output.status("    POST /forge/schemas");
    output.status("    GET  /forge/schemas");
    output.status("    GET  /forge/schemas/:name");
    output.status("    PUT  /forge/schemas/:name");
    output.status("    DEL  /forge/schemas/:name");
    output.status("    POST /forge/schemas/:schema/entities");
    output.status("    GET  /forge/schemas/:schema/entities");
    output.status("    GET  /forge/schemas/:schema/entities/:id");
    output.status("    PUT  /forge/schemas/:schema/entities/:id");
    output.status("    DEL  /forge/schemas/:schema/entities/:id");
    output.status("  Press Ctrl+C to stop.");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| CliError::Server {
            message: format!("server error: {e}"),
        })?;

    output.success("Server shut down gracefully.");
    Ok(())
}

/// Build the axum router with forge routes and health endpoint.
fn build_router(extension: &SchemaForgeExtension) -> Router {
    let router = Router::new().route("/health", get(health_handler));
    extension.register_routes(router)
}

/// Health check handler.
async fn health_handler() -> &'static str {
    "ok"
}

/// Wait for Ctrl+C to signal graceful shutdown.
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use schema_forge_surrealdb::SurrealBackend;
    use tower::ServiceExt;

    /// Build a test router backed by an in-memory SurrealDB.
    async fn test_router() -> Router {
        let backend = SurrealBackend::connect_memory("test", "serve_test")
            .await
            .expect("in-memory backend");
        let extension = SchemaForgeExtension::builder()
            .with_backend(backend)
            .build()
            .await
            .expect("extension");
        build_router(&extension)
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let router = test_router().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
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
    fn health_handler_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<std::pin::Pin<Box<dyn std::future::Future<Output = &'static str> + Send>>>();
    }

    #[test]
    fn build_router_returns_router() {
        // Compile-time verification that build_router produces a Router
        let _: fn(&SchemaForgeExtension) -> Router = build_router;
    }
}
