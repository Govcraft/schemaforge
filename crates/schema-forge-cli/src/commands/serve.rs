use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acton_service::prelude::ActorHandleInterface;
use acton_service::service_builder::ServiceBuilder;
use acton_service::versioning::{ApiVersion, VersionedApiBuilder};
use schema_forge_acton::hooks::{HookDispatcher, TonicDispatcherConfig, TonicHookDispatcher};
use schema_forge_acton::{
    DynForgeBackend, ForgeActor, InitForge, InitForgeData, ReplyChannel, SchemaForgeExtension,
};
use schema_forge_core::migration::DiffEngine;
use tokio::sync::oneshot;

use crate::cli::{GlobalOpts, ServeArgs};
use crate::commands::parse::parse_all_schemas;
use crate::config::{load_config, resolve_db_params, DbParams};
use crate::error::CliError;
use crate::output::OutputContext;

/// Maximum number of database connection retries before failing.
const MAX_CONNECT_RETRIES: u32 = 3;

/// Base delay in seconds between connection retries (doubles each attempt).
const CONNECT_BASE_DELAY_SECS: u64 = 2;

/// Timeout for the InitForge actor message round-trip.
const INIT_FORGE_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the `serve` command: start the SchemaForge HTTP server.
///
/// Loads configuration, parses schemas, connects to the database backend,
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

    // 3. Connect to database (try remote, fail explicitly for production)
    let connected = connect_with_retries(&db_params, output).await?;
    let backend_arc = connected.backend;
    let auth_store = connected.auth_store;

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
                output.status(&format!("  Applied {}", schema.name.as_str()));
            }

            // Always store metadata so the backend's SchemaId matches the
            // runtime registry. Each parse generates a new SchemaId, and
            // entity queries resolve table names via SchemaId lookup.
            backend_arc
                .store_schema_metadata(schema)
                .await
                .map_err(CliError::Backend)?;

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
        hook_dispatcher: None,
    };

    // 6. Warn about --watch
    if args.watch {
        output.warn("--watch is not yet implemented; schemas will not auto-reload.");
    }

    // Resolve runtime UI toggles: config.toml defaults, CLI flags override
    let enable_admin = config.server.admin_ui && !args.no_admin_ui;
    let enable_widget = config.server.widget_ui && !args.no_widget_ui;
    let enable_site = args.with_htmx;

    // Scaffold site templates and static assets when --with-htmx is active
    if enable_site {
        scaffold_site_assets(Path::new("."), output)?;
    }

    // 7. Build SchemaForgeExtension for admin/widget/site UI routes (shared session layer)
    let extension = if enable_admin || enable_widget || enable_site {
        let mut builder = SchemaForgeExtension::builder()
            .with_backend_arc(init_data.backend.clone())
            .with_auth_store_arc(auth_store);
        if let Some(ref password) = args.admin_password {
            builder = builder.with_admin_credentials(args.admin_user.clone(), password.clone());
        }
        if let Some(ref dir) = args.template_dir {
            builder = builder.with_template_dir(dir.clone());
        }
        if enable_site {
            builder = builder
                .with_site_template_dir(std::path::PathBuf::from("site/templates"))
                .with_site_static_dir(std::path::PathBuf::from("site/static"));
        }
        Some(builder.build().await.map_err(|e| CliError::Server {
            message: format!("failed to build SchemaForgeExtension: {e}"),
        })?)
    } else {
        None
    };

    // 8. Build versioned routes via acton-service, with optional frontend routes
    let routes =
        build_versioned_routes(extension.as_ref(), enable_admin, enable_widget, enable_site);

    // 8. Configure and serve via acton-service
    // Load from config.toml (picks up [token] section), then override serve-specific fields
    let mut svc_config =
        acton_service::config::Config::<schema_forge_acton::SchemaForgeConfig>::load_for_service(
            "schemaforge",
        )
        .unwrap_or_default();
    svc_config.service.port = args.port;
    svc_config.service.name = "schemaforge".to_string();

    #[cfg(feature = "surrealdb")]
    if let DbParams::Surrealdb(_) = &db_params {
        svc_config.surrealdb = Some(build_surrealdb_config(&db_params));
    }

    // Add frontend route prefixes to token auth's public_paths so the PASETO/JWT
    // middleware passes through without rejecting session-based browser requests.
    if enable_admin || enable_widget || enable_site {
        let mut public_paths = Vec::new();
        if enable_admin {
            public_paths.push("/admin".to_string());
        }
        if enable_widget {
            public_paths.push("/forge".to_string());
        }
        if enable_site {
            public_paths.push("/site".to_string());
        }
        if let Some(acton_service::config::TokenConfig::Paseto(ref mut pc)) = svc_config.token {
            pc.public_paths.extend(public_paths);
        }
    }

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
    if enable_admin {
        output.status("    GET  /admin/");
    }
    if enable_widget {
        output.status("    GET  /forge/{schema}/entities");
    }
    if enable_site {
        output.status("    GET  /site/");
    }
    output.status("  Press Ctrl+C to stop.");

    // Build service with ForgeActor registered as an actor extension
    let service = ServiceBuilder::new()
        .with_config(svc_config)
        .with_actor::<ForgeActor>()
        .with_actor::<schema_forge_acton::HookDispatchActor>()
        .with_routes(routes)
        .build();

    // Initialize the ForgeActor with runtime state (must happen before serving)
    let forge_handle = service
        .state()
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered after ServiceBuilder::build()");

    // Build the hook dispatcher from the resolved schema-forge config now that
    // svc_config has been finalized. This loads every binding's descriptor and
    // resolves the per-event service+method up front, so misconfiguration
    // surfaces immediately rather than on the first hooked CRUD call.
    let hooks_cfg = service.config().custom.schema_forge.hooks.clone();
    let hook_dispatcher: Option<Arc<dyn HookDispatcher>> =
        if hooks_cfg.enabled && !hooks_cfg.bindings.is_empty() {
            match TonicHookDispatcher::new(&hooks_cfg, TonicDispatcherConfig::default()) {
                Ok(d) => {
                    output.status(&format!(
                        "  Hook dispatcher initialized with {} binding(s).",
                        d.binding_count()
                    ));
                    Some(Arc::new(d) as Arc<dyn HookDispatcher>)
                }
                Err(e) => {
                    return Err(CliError::Server {
                        message: format!("failed to build hook dispatcher: {e}"),
                    });
                }
            }
        } else {
            init_data.hook_dispatcher
        };

    let (tx, rx) = oneshot::channel();
    forge_handle
        .send(InitForge {
            registry: init_data.registry,
            backend: init_data.backend,
            tenant_config: init_data.tenant_config,
            record_access_policy: init_data.record_access_policy,
            hook_dispatcher,
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

/// Connect to database with exponential backoff retries.
///
/// Unlike `connect_backend()` (used by CLI commands), this does NOT fall back
/// to in-memory on failure. A production server must connect to its configured
/// database or fail explicitly.
async fn connect_with_retries(
    db_params: &DbParams,
    output: &OutputContext,
) -> Result<ConnectedBackend, CliError> {
    let base_delay = Duration::from_secs(CONNECT_BASE_DELAY_SECS);
    let mut last_err = None;

    for attempt in 0..=MAX_CONNECT_RETRIES {
        match connect_once(db_params).await {
            Ok(connected) => {
                if attempt > 0 {
                    output.success(&format!(
                        "Connected to {} after {} attempt(s)",
                        db_params.url(),
                        attempt + 1
                    ));
                } else {
                    output.success(&format!("Connected to {}", db_params.url()));
                }
                return Ok(connected);
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
            db_params.url(),
            MAX_CONNECT_RETRIES + 1,
            last_err.unwrap(),
        ),
    })
}

/// Connected backend: the type-erased backend plus an optional auth store.
///
/// Both are produced from the same concrete backend at connection time, before
/// the concrete type is erased. This avoids needing the concrete type later
/// when building `SchemaForgeExtension` for admin/widget UI routes.
struct ConnectedBackend {
    backend: Arc<dyn DynForgeBackend>,
    auth_store: Arc<dyn schema_forge_acton::DynAuthStore>,
}

/// Attempt a single connection to the configured backend.
async fn connect_once(db_params: &DbParams) -> Result<ConnectedBackend, CliError> {
    match db_params {
        #[cfg(feature = "surrealdb")]
        DbParams::Surrealdb(p) => {
            let backend = schema_forge_surrealdb::SurrealBackend::connect_with_auth(
                &p.url,
                &p.namespace,
                &p.database,
                p.username.as_deref(),
                p.password.as_deref(),
            )
            .await
            .map_err(|e| CliError::Server {
                message: format!("SurrealDB connection failed: {e}"),
            })?;
            let backend = Arc::new(backend);
            Ok(ConnectedBackend {
                backend: backend.clone(),
                auth_store: backend,
            })
        }
        #[cfg(feature = "postgres")]
        DbParams::Postgres(p) => {
            let backend = schema_forge_postgres::PgBackend::connect(&p.url)
                .await
                .map_err(|e| CliError::Server {
                    message: format!("PostgreSQL connection failed: {e}"),
                })?;
            let backend = Arc::new(backend);
            Ok(ConnectedBackend {
                backend: backend.clone(),
                auth_store: backend,
            })
        }
        #[allow(unreachable_patterns)]
        other => Err(CliError::Config {
            message: format!("backend '{}' is not enabled in this build", other.url()),
        }),
    }
}

/// Build an acton-service `SurrealDbConfig` from resolved CLI database parameters.
///
/// This enables acton-service's health endpoint to report SurrealDB connection
/// status. Only available when the `surrealdb` feature is enabled.
#[cfg(feature = "surrealdb")]
fn build_surrealdb_config(db_params: &DbParams) -> acton_service::config::SurrealDbConfig {
    match db_params {
        DbParams::Surrealdb(p) => acton_service::config::SurrealDbConfig {
            url: p.url.clone(),
            namespace: p.namespace.clone(),
            database: p.database.clone(),
            username: p.username.clone(),
            password: p.password.clone(),
            max_retries: MAX_CONNECT_RETRIES,
            retry_delay_secs: CONNECT_BASE_DELAY_SECS,
            optional: false,
            lazy_init: false,
        },
        _ => unreachable!("build_surrealdb_config called with non-SurrealDB params"),
    }
}

/// Build versioned routes using acton-service's VersionedApiBuilder.
///
/// Nests SchemaForge's API routes under `/api/v1/forge/`. When a
/// `SchemaForgeExtension` is provided, admin and/or widget frontend routes
/// are mounted alongside the API routes via `with_frontend_routes()`,
/// sharing a single session layer for browser-based authentication.
fn build_versioned_routes(
    extension: Option<&SchemaForgeExtension>,
    enable_admin: bool,
    enable_widget: bool,
    enable_site: bool,
) -> acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig> {
    let mut builder = VersionedApiBuilder::<schema_forge_acton::SchemaForgeConfig>::with_config()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, |router| {
            SchemaForgeExtension::versioned_forge_routes(router)
        });

    if let Some(ext) = extension {
        let ext_admin = if enable_admin {
            Some(ext.admin_frontend_router())
        } else {
            None
        };
        let ext_widget = if enable_widget {
            Some(ext.widget_frontend_router())
        } else {
            None
        };
        let ext_site = if enable_site {
            Some(ext.site_frontend_router())
        } else {
            None
        };
        let session_layer = ext.session_layer();
        builder = builder.with_frontend_routes(move |router| {
            let mut r = router;
            if let Some(admin_router) = ext_admin {
                use axum::response::Redirect;
                use axum::routing::get;
                r = r
                    .nest_service("/admin/", admin_router)
                    .route("/admin", get(|| async { Redirect::permanent("/admin/") }));
            }
            if let Some(widget_router) = ext_widget {
                r = r.nest_service("/forge", widget_router);
            }
            if let Some(site_router) = ext_site {
                use axum::response::Redirect;
                use axum::routing::get;
                r = r
                    .nest_service("/site/", site_router)
                    .route("/site", get(|| async { Redirect::permanent("/site/") }));
            }
            r.layer(session_layer)
        });
    }

    builder.build_routes()
}

/// Scaffold starter HTMX site templates and static assets.
///
/// Creates `{project_dir}/site/templates/` and `{project_dir}/site/static/`
/// with embedded defaults. Each directory is only scaffolded if it does not
/// already exist, so user customizations are never overwritten.
fn scaffold_site_assets(project_dir: &Path, output: &OutputContext) -> Result<(), CliError> {
    // Scaffold templates
    let template_dir = project_dir.join("site/templates");
    if !template_dir.exists() {
        std::fs::create_dir_all(&template_dir).map_err(|e| CliError::Io {
            path: template_dir.clone(),
            source: e,
        })?;
        let templates: &[(&str, &str)] = &[
            (
                "base.html",
                include_str!("../../../schema-forge-acton/templates/site/base.html"),
            ),
            (
                "index.html",
                include_str!("../../../schema-forge-acton/templates/site/index.html"),
            ),
            (
                "login.html",
                include_str!("../../../schema-forge-acton/templates/site/login.html"),
            ),
            (
                "login_card.html",
                include_str!("../../../schema-forge-acton/templates/site/login_card.html"),
            ),
            (
                "entities.html",
                include_str!("../../../schema-forge-acton/templates/site/entities.html"),
            ),
            (
                "entity_detail.html",
                include_str!("../../../schema-forge-acton/templates/site/entity_detail.html"),
            ),
            (
                "entity_form.html",
                include_str!("../../../schema-forge-acton/templates/site/entity_form.html"),
            ),
        ];
        for (name, content) in templates {
            std::fs::write(template_dir.join(name), content).map_err(|e| CliError::Io {
                path: template_dir.join(name),
                source: e,
            })?;
        }
        output.status("  Scaffolded site templates into site/templates/");
    }

    // Scaffold static assets
    let static_dir = project_dir.join("site/static");
    if !static_dir.exists() {
        std::fs::create_dir_all(&static_dir).map_err(|e| CliError::Io {
            path: static_dir.clone(),
            source: e,
        })?;
        let assets: &[(&str, &str)] = &[(
            "site.css",
            include_str!("../../../schema-forge-acton/static/css/site.css"),
        )];
        for (name, content) in assets {
            std::fs::write(static_dir.join(name), content).map_err(|e| CliError::Io {
                path: static_dir.join(name),
                source: e,
            })?;
        }
        output.status("  Scaffolded site static assets into site/static/");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_versioned_routes_is_callable() {
        // Compile-time verification: builds routes without an extension
        let _routes = build_versioned_routes(None, false, false, false);
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn build_surrealdb_config_from_db_params() {
        use crate::config::SurrealDbParams;

        let db_params = DbParams::Surrealdb(SurrealDbParams {
            url: "ws://db.example.com:8000".to_string(),
            namespace: "production".to_string(),
            database: "main".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
        });

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

    fn test_output() -> OutputContext {
        OutputContext {
            mode: crate::output::OutputMode::Plain,
            verbose: 0,
            quiet: true,
            use_color: false,
        }
    }

    #[test]
    fn scaffold_creates_site_templates_and_static() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_site_assets(dir.path(), &test_output()).unwrap();
        assert!(dir.path().join("site/templates/base.html").exists());
        assert!(dir.path().join("site/templates/index.html").exists());
        assert!(dir.path().join("site/templates/login.html").exists());
        assert!(dir.path().join("site/static/site.css").exists());
    }

    #[test]
    fn scaffold_skips_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-create both directories with custom content
        let template_dir = dir.path().join("site/templates");
        std::fs::create_dir_all(&template_dir).unwrap();
        std::fs::write(template_dir.join("base.html"), "custom").unwrap();
        let static_dir = dir.path().join("site/static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("site.css"), "/* custom */").unwrap();

        scaffold_site_assets(dir.path(), &test_output()).unwrap();

        // Should not overwrite either directory
        let content = std::fs::read_to_string(template_dir.join("base.html")).unwrap();
        assert_eq!(content, "custom");
        let css = std::fs::read_to_string(static_dir.join("site.css")).unwrap();
        assert_eq!(css, "/* custom */");
    }

    #[cfg(feature = "surrealdb")]
    #[test]
    fn build_surrealdb_config_without_credentials() {
        use crate::config::SurrealDbParams;

        let db_params = DbParams::Surrealdb(SurrealDbParams {
            url: "mem://".to_string(),
            namespace: "test".to_string(),
            database: "test".to_string(),
            username: None,
            password: None,
        });

        let config = build_surrealdb_config(&db_params);

        assert_eq!(config.url, "mem://");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }
}
