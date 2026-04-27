use std::sync::Arc;
use std::time::Duration;

use acton_service::auth::config::{PasetoGenerationConfig, TokenGenerationConfig};
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
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
use crate::config::{load_svc_config, resolve_db_params, DbParams};
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
    // 1. Load the canonical acton-service config and apply CLI overrides.
    //    Both schema-forge's backend connection and acton-service's pool
    //    read from this single struct, so they cannot diverge (#47).
    //
    //    Loading here — before the connection-retry loop — also keeps
    //    issue #44's invariant: a malformed `[schema_forge.*]` section
    //    fails the boot in milliseconds with the underlying Figment
    //    error rather than minutes later behind a connect timeout.
    //    `load_svc_config` already propagates Figment errors verbatim.
    let mut svc_config = load_svc_config(global)?;
    let db_params = resolve_db_params(&svc_config)?;
    let storage_config = svc_config.custom.schema_forge.storage.clone();

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

    // 4. Connect to database (try remote, fail explicitly for production)
    let connected = connect_with_retries(&db_params, output).await?;
    let backend_arc = connected.backend.clone();
    let entity_store = connected.entity_store.clone();

    // 5. Build ForgeActor initialization data (loads schemas, seeds system schemas, builds tenant config)
    let init_data =
        SchemaForgeExtension::build_init(backend_arc.clone(), None, &storage_config)
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
        backend: backend_arc.clone(),
        tenant_config,
        record_access_policy: None,
        hook_dispatcher: None,
        storage_registry: init_data.storage_registry,
        policy_store: init_data.policy_store,
    };

    // Build the canonical AuthStore from the User entity table. This
    // is the production identity-store path: every user-mgmt mutation
    // flows through the User schema, with `password_hash` locked behind
    // `@hidden` so it never leaves the storage boundary. The legacy
    // `_forge_users` table is no longer touched.
    let auth_store = build_entity_auth_store(&init_data, entity_store.clone())?;

    // 6. Warn about --watch
    if args.watch {
        output.warn("--watch is not yet implemented; schemas will not auto-reload.");
    }

    // 7. Bootstrap the initial admin user, if requested. The
    //    SchemaForgeExtension builder is the legacy seam for this — it
    //    no longer mounts any routes; the JSON forge router is mounted
    //    directly by `build_versioned_routes()` below.
    if args.admin_password.is_some() {
        let builder = SchemaForgeExtension::builder()
            .with_backend_arc(init_data.backend.clone())
            .with_auth_store_arc(auth_store.clone())
            .with_storage_config(svc_config.custom.schema_forge.storage.clone())
            .with_admin_credentials(
                args.admin_user.clone(),
                args.admin_password.clone().unwrap_or_default(),
            );
        builder.build().await.map_err(|e| CliError::Server {
            message: format!("failed to build SchemaForgeExtension: {e}"),
        })?;
    }

    // Configure acton-service before building routes so we can read the token
    // config, mint a PasetoGenerator, and wire the login endpoint's Extension
    // layer onto the versioned router. `svc_config` was already loaded above
    // so the storage registry could be initialized; finalize the remaining
    // fields here. Database/SurrealDB sections are not touched here — they
    // were resolved up-front by `load_svc_config` so acton-service's pool
    // and the schema-forge backend pool see the same URL by construction.
    svc_config.service.port = args.port;
    svc_config.service.name = "schemaforge".to_string();

    // Token auth public path: the JSON login endpoint must be reachable
    // without a bearer token so clients can obtain one.
    if let Some(acton_service::config::TokenConfig::Paseto(ref mut pc)) = svc_config.token {
        pc.public_paths.push("/api/v1/forge/auth/login".to_string());
    }

    // Opt-in permissive CORS for local development. Warns loudly in logs.
    if args.dev_cors {
        tracing::warn!(
            "dev CORS is enabled — allowing all origins. DO NOT use this in production."
        );
        svc_config.with_development_cors();
    } else if svc_config.middleware.cors_mode == "permissive" {
        tracing::warn!(
            "config.toml sets [middleware] cors_mode = \"permissive\" — allowing all origins. \
             DO NOT use this in production."
        );
    }

    // Build the PASETO generator using the same key file that the token
    // middleware will use to validate incoming tokens. The key file is
    // auto-created on first boot when missing so `serve` is self-bootstrapping.
    let paseto_generator = build_paseto_generator(&svc_config, output)?;

    // 8. Build versioned routes via acton-service for the JSON forge API.
    let login_auth_store: Arc<dyn schema_forge_acton::DynAuthStore> = auth_store.clone();
    let routes = build_versioned_routes(login_auth_store, paseto_generator);

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
            storage_registry: init_data.storage_registry,
            policy_store: init_data.policy_store,
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

/// Connected backend: the type-erased schema/entity backend plus the
/// trait-object-safe entity store handle that powers the
/// [`schema_forge_backend::EntityAuthStore`].
///
/// Both handles are produced from the same concrete backend at connection
/// time, before the concrete type is erased. The legacy
/// `schema_forge_acton::DynAuthStore` is no longer derived here — the
/// canonical auth store is built later from the User entity table once
/// the policy_store's role-rank table is in scope.
struct ConnectedBackend {
    backend: Arc<dyn DynForgeBackend>,
    entity_store: Arc<dyn schema_forge_backend::DynEntityStore>,
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
                entity_store: backend,
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
                entity_store: backend,
            })
        }
        #[allow(unreachable_patterns)]
        other => Err(CliError::Config {
            message: format!("backend '{}' is not enabled in this build", other.url()),
        }),
    }
}

/// Build the canonical auth store for the running server.
///
/// Returns an [`EntityAuthStore`] wrapped behind the acton-service
/// `DynAuthStore` trait so it slots straight into the existing
/// extension and login layers. The store reads and writes the `User`
/// entity table, with `password_hash` locked behind `@hidden`.
fn build_entity_auth_store(
    init_data: &InitForgeData,
    entity_store: Arc<dyn schema_forge_backend::DynEntityStore>,
) -> Result<Arc<dyn schema_forge_acton::DynAuthStore>, CliError> {
    let user_schema = init_data
        .registry
        .get("User")
        .cloned()
        .ok_or_else(|| CliError::Server {
            message:
                "User system schema is not registered; cannot build EntityAuthStore. \
                 Confirm `seed_system_schemas_into_map` ran during InitForgeData::build."
                    .into(),
        })?;

    let policy_store = init_data
        .policy_store
        .clone()
        .ok_or_else(|| CliError::Server {
            message: "policy_store missing from InitForgeData; cannot build EntityAuthStore"
                .into(),
        })?;

    let resolver: schema_forge_backend::entity_auth_store::RoleRankResolver =
        Arc::new(move |role: &str| policy_store.current().role_ranks.get(role));

    Ok(Arc::new(schema_forge_backend::EntityAuthStore::new(
        entity_store,
        user_schema,
        resolver,
    )))
}

/// Build a [`PasetoGenerator`] from the loaded acton-service config.
///
/// The generator shares the same key file as the token middleware so minted
/// tokens round-trip through validation. If the key file does not exist yet
/// (e.g. a fresh `mem://` smoke test before `schemaforge token init-key`
/// has been run) it is auto-generated via
/// [`crate::commands::token::ensure_paseto_key`].
///
/// Returns an error only when PASETO is not configured (e.g. the user has
/// disabled `[token]` in `config.toml`, which would also disable token auth
/// and therefore the login endpoint).
fn build_paseto_generator(
    svc_config: &acton_service::config::Config<schema_forge_acton::SchemaForgeConfig>,
    output: &OutputContext,
) -> Result<Arc<PasetoGenerator>, CliError> {
    let paseto_cfg = match &svc_config.token {
        Some(acton_service::config::TokenConfig::Paseto(pc)) => pc,
        _ => {
            return Err(CliError::Config {
                message: "[token] must be configured with format = \"paseto\" for the login \
                          endpoint to mint tokens"
                    .to_string(),
            });
        }
    };

    crate::commands::token::ensure_paseto_key(&paseto_cfg.key_path)?;
    if !paseto_cfg.key_path.exists() {
        return Err(CliError::Config {
            message: format!(
                "PASETO key file missing after ensure_paseto_key at {}",
                paseto_cfg.key_path.display()
            ),
        });
    }
    output.status(&format!(
        "  PASETO key loaded from {}",
        paseto_cfg.key_path.display()
    ));

    let paseto_gen_config = PasetoGenerationConfig {
        version: paseto_cfg.version.clone(),
        purpose: paseto_cfg.purpose.clone(),
        key_path: paseto_cfg.key_path.clone(),
        issuer: paseto_cfg.issuer.clone(),
        audience: paseto_cfg.audience.clone(),
    };
    let token_gen_config = TokenGenerationConfig {
        access_token_lifetime_secs: 3600,
        issuer: paseto_cfg
            .issuer
            .clone()
            .or_else(|| Some("schemaforge".to_string())),
        audience: paseto_cfg.audience.clone(),
        include_jti: true,
    };

    let generator = PasetoGenerator::new(&paseto_gen_config, &token_gen_config).map_err(|e| {
        CliError::Config {
            message: format!("failed to build PASETO generator: {e}"),
        }
    })?;
    Ok(Arc::new(generator))
}

/// Build versioned routes using acton-service's VersionedApiBuilder.
///
/// Nests SchemaForge's JSON API routes under `/api/v1/forge/`. All UI
/// surfaces are generated client-side by `schemaforge site generate`; this
/// server only serves the JSON API plus the login endpoint.
fn build_versioned_routes(
    auth_store: Arc<dyn schema_forge_acton::DynAuthStore>,
    paseto_generator: Arc<PasetoGenerator>,
) -> acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig> {
    // Cloned into the add_version closure so the login handler can
    // extract them via axum::Extension.
    let auth_store_layer = auth_store;
    let generator_layer = paseto_generator;
    VersionedApiBuilder::<schema_forge_acton::SchemaForgeConfig>::with_config()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, move |router| {
            use axum::Extension;
            SchemaForgeExtension::versioned_forge_routes(router)
                .layer(Extension(auth_store_layer))
                .layer(Extension(generator_layer))
        })
        .build_routes()
}

/// Mem-backed SurrealDB is the only auth store we can stand up synchronously
/// in-process, so the only test in this file is surrealdb-feature-gated.
/// Postgres builds get coverage from the resolver tests in `config.rs`.
#[cfg(all(test, feature = "surrealdb"))]
mod tests {
    use super::*;

    #[test]
    fn build_versioned_routes_is_callable() {
        // Compile-time verification: builds routes without an extension.
        // A dummy PasetoGenerator is constructed from a fixed 32-byte
        // symmetric key so we don't need a key file on disk.
        use acton_service::auth::config::TokenGenerationConfig;
        use schema_forge_backend::EntityAuthStore;
        use schema_forge_core::types::{
            FieldAnnotation, FieldDefinition, FieldModifier, FieldName, FieldType,
            IntegerConstraints, SchemaDefinition, SchemaId, SchemaName, TextConstraints,
        };
        use schema_forge_surrealdb::SurrealBackend;

        let key = [0u8; 32];
        let generator = Arc::new(PasetoGenerator::with_symmetric_key(
            key,
            TokenGenerationConfig::default(),
        ));

        let rt = tokio::runtime::Runtime::new().unwrap();
        let backend = rt
            .block_on(SurrealBackend::connect_with_auth(
                "mem://", "test", "test", None, None,
            ))
            .unwrap();
        let backend = Arc::new(backend);
        let entity_store: Arc<dyn schema_forge_backend::DynEntityStore> = backend.clone();

        // Minimal User schema mirroring the production system schema's
        // shape so the auth store has a valid SchemaDefinition handle.
        let user_schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("User").unwrap(),
            vec![
                FieldDefinition::with_annotations(
                    FieldName::new("email").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("display_name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::new(
                    FieldName::new("roles").unwrap(),
                    FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("role_rank").unwrap(),
                    FieldType::Integer(IntegerConstraints::default()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::new(
                    FieldName::new("active").unwrap(),
                    FieldType::Boolean,
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("password_hash").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![],
                    vec![FieldAnnotation::Hidden],
                ),
            ],
            Vec::new(),
        )
        .unwrap();

        let resolver: schema_forge_backend::entity_auth_store::RoleRankResolver =
            Arc::new(|_role: &str| None);
        let auth_store: Arc<dyn schema_forge_acton::DynAuthStore> = Arc::new(
            EntityAuthStore::new(entity_store, user_schema, resolver),
        );

        let _routes = build_versioned_routes(auth_store, generator);
    }
}
