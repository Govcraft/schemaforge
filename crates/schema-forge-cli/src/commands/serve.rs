use std::sync::Arc;
use std::time::Duration;

use acton_service::prelude::ActorHandleInterface;
use acton_service::service_builder::ServiceBuilder;
use acton_service::versioning::{ApiVersion, VersionedApiBuilder};
use schema_forge_acton::{
    ForgeActor, InitForge, InitForgeData, ReplyChannel, SchemaForgeExtension,
};
use schema_forge_core::migration::DiffEngine;
use schema_forge_surrealdb::SurrealBackend;
use tokio::sync::oneshot;

use crate::cli::{GlobalOpts, ServeArgs};
use crate::commands::parse::parse_all_schemas;
use crate::config::{load_config, resolve_db_params};
use crate::error::CliError;
use crate::output::OutputContext;

/// Maximum number of SurrealDB connection retries before failing.
const MAX_CONNECT_RETRIES: u32 = 3;

/// Base delay in seconds between connection retries (doubles each attempt).
const CONNECT_BASE_DELAY_SECS: u64 = 2;

/// Timeout for the InitForge actor message round-trip.
const INIT_FORGE_TIMEOUT: Duration = Duration::from_secs(10);

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
    let backend = connect_with_retries(&db_params, output).await?;
    let backend_arc: Arc<dyn schema_forge_acton::DynForgeBackend> = Arc::new(backend);

    // 4. Build ForgeActor initialization data (loads schemas, seeds system schemas, builds tenant config)
    let init_data = SchemaForgeExtension::build_init(backend_arc.clone(), None)
        .await
        .map_err(|e| CliError::Server {
            message: format!("failed to build ForgeActor init data: {e}"),
        })?;

    // 5. Apply parsed schemas (using the backend directly, before actor spawning)
    let mut registry = init_data.registry;
    if !schemas.is_empty() {
        output.status("Applying schemas...");
        for schema in &schemas {
            let existing = backend_arc
                .load_schema_metadata(&schema.name)
                .await
                .map_err(CliError::Backend)?;

            let plan = if let Some(old) = existing {
                DiffEngine::diff(&old, schema)
            } else {
                DiffEngine::create_new(schema)
            };

            if !plan.is_empty() {
                backend_arc
                    .apply_migration(&schema.name, &plan.steps)
                    .await
                    .map_err(CliError::Backend)?;
                backend_arc
                    .store_schema_metadata(schema)
                    .await
                    .map_err(CliError::Backend)?;

                output.status(&format!("  Applied {}", schema.name.as_str()));
            }

            // Update registry (whether migrated or not — ensures parsed schemas are present)
            registry.insert(schema.name.as_str().to_string(), schema.clone());
        }
    }

    // Rebuild tenant config after applying parsed schemas
    let all_schemas: Vec<_> = registry.values().cloned().collect();
    let tenant_config = schema_forge_backend::tenant::TenantConfig::from_schemas(&all_schemas)
        .map_err(|e| CliError::Server {
            message: format!("Invalid tenant configuration: {e}"),
        })?;
    let tenant_config = if tenant_config.is_enabled() {
        Some(tenant_config)
    } else {
        None
    };

    let init_data = InitForgeData {
        registry,
        backend: backend_arc,
        tenant_config,
        record_access_policy: None,
    };

    // 6. Warn about --watch
    if args.watch {
        output.warn("--watch is not yet implemented; schemas will not auto-reload.");
    }

    // 7. Build versioned routes via acton-service
    let routes = build_versioned_routes();

    // 8. Configure and serve via acton-service
    // Load from config.toml (picks up [token] section), then override serve-specific fields
    let mut svc_config =
        acton_service::config::Config::<schema_forge_acton::SchemaForgeConfig>::load_for_service(
            "schema-forge",
        )
        .unwrap_or_default();
    svc_config.service.port = args.port;
    svc_config.service.name = "schema-forge".to_string();
    svc_config.surrealdb = Some(build_surrealdb_config(&db_params));

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
    #[cfg(feature = "widget-ui")]
    output.status("    GET  /forge/{schema}/entities");
    output.status("  Press Ctrl+C to stop.");

    // Build service with ForgeActor registered as an actor extension
    let service = ServiceBuilder::new()
        .with_config(svc_config)
        .with_actor::<ForgeActor>()
        .with_routes(routes)
        .build();

    // Initialize the ForgeActor with runtime state (must happen before serving)
    let forge_handle = service
        .state()
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered after ServiceBuilder::build()");

    let (tx, rx) = oneshot::channel();
    forge_handle
        .send(InitForge {
            registry: init_data.registry,
            backend: init_data.backend,
            tenant_config: init_data.tenant_config,
            record_access_policy: init_data.record_access_policy,
            reply: ReplyChannel::new(tx),
        })
        .await;

    // Wait for init to complete before serving requests
    tokio::time::timeout(INIT_FORGE_TIMEOUT, rx)
        .await
        .map_err(|_| CliError::Server {
            message: "ForgeActor initialization timed out".to_string(),
        })?
        .map_err(|_| CliError::Server {
            message: "ForgeActor initialization failed (channel dropped)".to_string(),
        })?;

    service.serve().await.map_err(|e| CliError::Server {
        message: format!("server error: {e}"),
    })?;

    output.success("Server shut down gracefully.");
    Ok(())
}

/// Connect to SurrealDB with exponential backoff retries.
///
/// Unlike `connect_backend()` (used by CLI commands), this does NOT fall back
/// to in-memory on failure. A production server must connect to its configured
/// database or fail explicitly.
async fn connect_with_retries(
    db_params: &crate::config::DbParams,
    output: &OutputContext,
) -> Result<SurrealBackend, CliError> {
    let base_delay = Duration::from_secs(CONNECT_BASE_DELAY_SECS);
    let mut last_err = None;

    for attempt in 0..=MAX_CONNECT_RETRIES {
        match SurrealBackend::connect_with_auth(
            &db_params.url,
            &db_params.namespace,
            &db_params.database,
            db_params.username.as_deref(),
            db_params.password.as_deref(),
        )
        .await
        {
            Ok(backend) => {
                if attempt > 0 {
                    output.success(&format!(
                        "Connected to {} after {} attempt(s)",
                        db_params.url,
                        attempt + 1
                    ));
                } else {
                    output.success(&format!("Connected to {}", db_params.url));
                }
                return Ok(backend);
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < MAX_CONNECT_RETRIES {
                    let delay = base_delay * 2_u32.pow(attempt);
                    output.warn(&format!(
                        "Connection attempt {} failed: {}. Retrying in {delay:?}...",
                        attempt + 1,
                        last_err.as_ref().unwrap(),
                    ));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    Err(CliError::Server {
        message: format!(
            "failed to connect to {} after {} attempts: {}",
            db_params.url,
            MAX_CONNECT_RETRIES + 1,
            last_err.unwrap(),
        ),
    })
}

/// Build an acton-service `SurrealDbConfig` from resolved CLI database parameters.
///
/// This enables acton-service's health endpoint to report SurrealDB connection
/// status. The actual connection is established by `connect_with_retries()` before
/// `ServiceBuilder::build()` because the extension needs a live client to
/// load schemas and seed system tables.
fn build_surrealdb_config(
    db_params: &crate::config::DbParams,
) -> acton_service::config::SurrealDbConfig {
    acton_service::config::SurrealDbConfig {
        url: db_params.url.clone(),
        namespace: db_params.namespace.clone(),
        database: db_params.database.clone(),
        username: db_params.username.clone(),
        password: db_params.password.clone(),
        max_retries: MAX_CONNECT_RETRIES,
        retry_delay_secs: CONNECT_BASE_DELAY_SECS,
        optional: false,
        lazy_init: false,
    }
}

/// Build versioned routes using acton-service's VersionedApiBuilder.
///
/// Nests SchemaForge's routes under `/api/v1/forge/`. Route handlers access the
/// `ForgeActor` via `state.actor::<ForgeActor>()` from `AppState`.
fn build_versioned_routes(
) -> acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig> {
    let builder =
        VersionedApiBuilder::<schema_forge_acton::SchemaForgeConfig>::with_config()
            .with_base_path("/api")
            .add_version(ApiVersion::V1, |router| {
                SchemaForgeExtension::versioned_forge_routes(router)
            });

    builder.build_routes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_versioned_routes_is_callable() {
        // Compile-time verification that build_versioned_routes has the right signature
        let _: fn()
            -> acton_service::service_builder::VersionedRoutes<
                schema_forge_acton::SchemaForgeConfig,
            > = build_versioned_routes;
    }

    #[test]
    fn build_surrealdb_config_from_db_params() {
        let db_params = crate::config::DbParams {
            url: "ws://db.example.com:8000".to_string(),
            namespace: "production".to_string(),
            database: "main".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
        };

        let config = build_surrealdb_config(&db_params);

        assert_eq!(config.url, "ws://db.example.com:8000");
        assert_eq!(config.namespace, "production");
        assert_eq!(config.database, "main");
        assert_eq!(config.username, Some("admin".to_string()));
        assert_eq!(config.password, Some("secret".to_string()));
        assert_eq!(config.max_retries, MAX_CONNECT_RETRIES);
        assert_eq!(config.retry_delay_secs, CONNECT_BASE_DELAY_SECS);
        assert!(!config.optional);
        assert!(!config.lazy_init);
    }

    #[test]
    fn build_surrealdb_config_without_credentials() {
        let db_params = crate::config::DbParams {
            url: "mem://".to_string(),
            namespace: "test".to_string(),
            database: "test".to_string(),
            username: None,
            password: None,
        };

        let config = build_surrealdb_config(&db_params);

        assert_eq!(config.url, "mem://");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }
}
