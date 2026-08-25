use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use acton_service::auth::config::{PasetoGenerationConfig, TokenGenerationConfig};
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::middleware::paseto::PasetoAuth;
use acton_service::prelude::ActorHandleInterface;
use acton_service::service_builder::ServiceBuilder;
use acton_service::versioning::{ApiVersion, VersionedApiBuilder};
use schema_forge_acton::hooks::{
    HookCredentialSource, HookDispatcher, PasetoHookCredential, TonicDispatcherConfig,
    TonicHookDispatcher,
};
use schema_forge_acton::{
    DynForgeBackend, ForgeActor, InitForge, InitForgeData, ReplyChannel, SchemaForgeExtension,
};
use schema_forge_core::migration::DiffEngine;
use tokio::sync::oneshot;

use crate::cli::{GlobalOpts, ServeArgs};
use crate::commands::parse::{load_verify_policy, parse_all_schemas};
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
    let verify_policy = load_verify_policy(global, output)?;
    let schemas = match parse_all_schemas(
        std::slice::from_ref(&args.schema_dir),
        &verify_policy,
        output,
    ) {
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

    // Load the role-rank hierarchy so the runtime no-upward-visibility
    // guard runs against the operator-controlled file the same way
    // `policies validate` does. Missing or invalid file aborts startup —
    // we refuse to silently degrade to an empty hierarchy.
    let role_ranks = schema_forge_acton::authz::RoleRanks::from_toml_file(&args.role_ranks)
        .map_err(|e| CliError::Server {
            message: format!(
                "failed to load role ranks from {}: {e}",
                args.role_ranks.display()
            ),
        })?;

    // 4b. Resolve operator-defined PASETO custom-claim → Cedar principal
    //     attribute mappings from `[schema_forge.authz.principal_claims]`.
    //     Empty when the operator has not configured the section, preserving
    //     pre-#50 behaviour.
    let principal_claims = schema_forge_acton::authz::PrincipalClaimMappings::from_config(
        &svc_config.custom.schema_forge.authz.principal_claims,
    )
    .map_err(|e| CliError::Server {
        message: format!("invalid [schema_forge.authz.principal_claims]: {e}"),
    })?;

    // 4c. Resolve the custom-policies directory. CLI `--custom-dir` overrides
    //     `[schema_forge.authz] custom_policies_dir` in config.toml. When
    //     neither is set, auto-discover `policies/custom` relative to the
    //     working directory only when that directory exists, so deployments
    //     without the convention don't generate a misleading log line. An
    //     explicitly-named missing directory is still passed through —
    //     `load_custom_policies` treats it as empty — but we warn so the
    //     misconfiguration is visible. Fixes issue #57.
    let custom_dir = resolve_custom_policies_dir(
        args.custom_dir.as_deref(),
        svc_config
            .custom
            .schema_forge
            .authz
            .custom_policies_dir
            .as_deref(),
    );
    if let Some(dir) = custom_dir.as_deref() {
        if !dir.exists() {
            output.warn(&format!(
                "custom policies directory {} does not exist; bundle will use generated policies only",
                dir.display()
            ));
        }
    }

    // 5. Build ForgeActor initialization data (loads schemas, seeds system schemas, builds tenant config)
    let init_data = SchemaForgeExtension::build_init(
        backend_arc.clone(),
        None,
        &storage_config,
        role_ranks,
        principal_claims,
        custom_dir.as_deref(),
    )
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

    // Recompile the Cedar policy bundle now that --schemas have been merged
    // into the registry. `build_init` ran before the parsed schemas were
    // applied, so its initial PolicyStore covers only the system schemas;
    // without this step the runtime would reject every authz check against
    // an app schema with "type X is not declared in the schema".
    if let Some(policy_store) = &init_data.policy_store {
        policy_store
            .recompile_from_schemas(&all_schemas, custom_dir.as_deref())
            .map_err(|e| CliError::Server {
                message: format!("Cedar policy recompile failed after schema apply: {e}"),
            })?;

        // Log the final bundle posture so misconfiguration (missing custom
        // policies, wrong directory) is obvious at startup. Mirrors the
        // success line `schemaforge policies validate` already prints.
        let snap = policy_store.current();
        let custom_suffix = custom_dir
            .as_deref()
            .map(|d| format!(" (custom: {})", d.display()))
            .unwrap_or_default();
        output.success(&format!(
            "Cedar bundle loaded: {} policies, hash {}{custom_suffix}",
            snap.policy_count,
            &snap.policy_hash[..16],
        ));
    }

    // Phase-2 principal-claim validation: bind every `source.user_field`
    // declaration to the *effective* User schema in the registry, after any
    // operator override from the parsed schemas has been merged. Misconfigured
    // mappings abort startup — refuse to silently drop a deployment-declared
    // contract.
    let mut resolved_principal_claims = (*init_data.principal_claims).clone();
    let user_schema = registry.get("User").ok_or_else(|| CliError::Server {
        message: "User schema is not registered; cannot validate principal-claim sources"
            .to_string(),
    })?;
    resolved_principal_claims
        .resolve_user_field_sources(user_schema)
        .map_err(|e| CliError::Server {
            message: format!("invalid [schema_forge.authz.principal_claims] source binding: {e}"),
        })?;
    let resolved_principal_claims = Arc::new(resolved_principal_claims);

    let init_data = InitForgeData {
        registry,
        backend: backend_arc.clone(),
        tenant_config,
        record_access_policy: None,
        hook_dispatcher: None,
        storage_registry: init_data.storage_registry,
        policy_store: init_data.policy_store,
        principal_claims: resolved_principal_claims.clone(),
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
        // Re-resolve the principal-claim mappings for the bootstrap builder so
        // its internal Cedar compile sees the same attribute set as the daemon
        // — otherwise a custom policy referencing an operator-mapped attribute
        // would fail strict validation here even though the running server is
        // configured to accept it.
        let bootstrap_principal_claims =
            schema_forge_acton::authz::PrincipalClaimMappings::from_config(
                &svc_config.custom.schema_forge.authz.principal_claims,
            )
            .map_err(|e| CliError::Server {
                message: format!("invalid [schema_forge.authz.principal_claims]: {e}"),
            })?;

        let builder = SchemaForgeExtension::builder()
            .with_backend_arc(init_data.backend.clone())
            .with_auth_store_arc(auth_store.clone())
            .with_storage_config(svc_config.custom.schema_forge.storage.clone())
            .with_principal_claims(bootstrap_principal_claims)
            .with_seed_demo_users(args.seed_demo_users)
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

    // Token auth public paths: both endpoints must be reachable without a
    // bearer token. `/auth/login` so clients can obtain one; `/meta` so the
    // login screen can display real backend / auth / build values before
    // the user has any token at all.
    if let Some(acton_service::config::TokenConfig::Paseto(ref mut pc)) = svc_config.token {
        pc.public_paths.push("/api/v1/forge/auth/login".to_string());
        pc.public_paths.push("/api/v1/forge/meta".to_string());
        // The invitee has no token yet; the accept endpoint authenticates by
        // possession of a valid, unconsumed invitation, not a bearer.
        pc.public_paths
            .push("/api/v1/forge/auth/invites/accept".to_string());
        // The bundled ops console (when embedded) is a static SPA served under
        // `/console`; its shell + assets + client-side routes must load before
        // the user has a token (they sign in *through* it). The prefix is
        // disjoint from `/api`, so the JSON API stays bearer-protected.
        #[cfg(feature = "embedded-console")]
        if !args.no_console {
            pc.public_paths.push("/console".to_string());
        }
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

    // Build the PASETO *validator* from the same config/key the generator
    // uses. The invite-accept endpoint re-verifies the stored invite token
    // through this validator so role/tenant are read from signed claims.
    let paseto_validator = build_paseto_validator(&svc_config)?;

    // Provision the internal ForgeInvitation table (NOT registered in the
    // public schema registry) and build the invite store over it.
    let invite_store = schema_forge_acton::system::provision_invite_store(
        backend_arc.as_ref(),
        entity_store.clone(),
    )
    .await
    .map_err(|e| CliError::Server {
        message: format!("failed to provision invite store: {e}"),
    })?;

    // Build the outbound email transport. When `[schema_forge.email]` is
    // disabled we still inject a sender — one that fails closed — so the
    // invite endpoints return a clear "email not configured" error rather
    // than 500-ing on a missing extension.
    let mut email_cfg = svc_config.custom.schema_forge.email.clone();
    // The SMTP password must never live in committed TOML. acton-service's
    // `ACTON_`-prefixed Figment env layering can't target the `[schema_forge]`
    // section — `Env::split("_")` shatters the underscore in the section key
    // — so the secret is accepted through a dedicated env var instead,
    // matching the `SCHEMAFORGE_*` convention used elsewhere (token, trust
    // policy). Set `SCHEMAFORGE_SMTP_PASSWORD` to authenticate the relay.
    if let Some(pw) = std::env::var("SCHEMAFORGE_SMTP_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
    {
        email_cfg.password = Some(pw);
    }
    let project_name = &svc_config.custom.schema_forge.project_name;
    let email_sender: Arc<dyn schema_forge_acton::email::EmailSender> = if email_cfg.enabled {
        Arc::new(
            schema_forge_acton::email::SmtpEmailSender::from_config(&email_cfg, project_name)
                .map_err(|e| CliError::Config {
                    message: format!("invalid [schema_forge.email] config: {e}"),
                })?,
        )
    } else {
        Arc::new(schema_forge_acton::email::DisabledEmailSender::new(
            email_cfg.public_base_url.clone(),
        ))
    };

    // 8. Build versioned routes via acton-service for the JSON forge API.
    //    Build a `MetaInfo` snapshot from the resolved DB params + the
    //    login token TTL so `GET /api/v1/forge/meta` can surface honest
    //    runtime posture to unauthenticated callers (the login screen).
    let login_auth_store: Arc<dyn schema_forge_acton::DynAuthStore> = auth_store.clone();
    let meta_info = build_meta_info(&db_params);
    let tenant_config_layer: Arc<Option<schema_forge_backend::tenant::TenantConfig>> =
        Arc::new(init_data.tenant_config.clone());
    let tenant_scope_state = schema_forge_acton::middleware::tenant_scope::TenantScopeState {
        entity_store: entity_store.clone(),
        tenant_config: tenant_config_layer.clone(),
    };
    // Hook calls are authenticated with a credential minted from this same
    // generator, so a hook service validates them with the `[token]` section
    // it would use for any other acton-service surface — no second key to
    // distribute, and no long-lived shared secret in config. Cloned before the
    // generator is moved into the route builder.
    let hook_credential: Arc<dyn HookCredentialSource> =
        Arc::new(PasetoHookCredential::new(paseto_generator.clone()));
    let routes = build_versioned_routes(
        login_auth_store,
        paseto_generator,
        paseto_validator,
        invite_store,
        email_sender,
        meta_info,
        resolved_principal_claims,
        tenant_config_layer,
        tenant_scope_state,
    );

    // LOCAL WORKAROUND for issue #55: `acton-service`'s default `/health`
    // handler reports `env!("CARGO_PKG_VERSION")` of the `acton-service`
    // crate (currently 0.23.x), not the running `schema-forge-acton`
    // version. That mismatch made `/health` drift from
    // `/api/v1/forge/meta` and from `schema-forge --version`.
    //
    // `acton-service 0.23` exposes no version-override hook, so we layer
    // a SchemaForge middleware that intercepts `GET /health` before the
    // upstream handler runs and replies with the correct version. Other
    // requests (notably `/ready` and `/api/v1/...`) pass through.
    //
    // TODO: remove once acton-service supports a service-version override
    // (e.g. `Config.service.version` or a `with_service_version` builder).
    let routes = wrap_health_with_schema_forge_version(routes);

    // Mount the embedded ops console as the root fallback (served same-origin at
    // `/`), unless this build omitted it or the operator passed --no-console.
    #[cfg(feature = "embedded-console")]
    let routes = if args.no_console {
        routes
    } else {
        mount_console(routes)
    };
    let console_served = cfg!(feature = "embedded-console") && !args.no_console;

    let bind_addr = format!("{}:{}", args.host, args.port);
    output.success(&format!(
        "SchemaForge server listening on http://{bind_addr}"
    ));
    if console_served {
        output.status(&format!("  Console → http://{bind_addr}/console"));
    }
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
        .with_actor::<schema_forge_acton::ExportJobActor>()
        .with_actor::<schema_forge_acton::ExportRateLimiter>()
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
            let dispatcher_cfg = TonicDispatcherConfig {
                credential: Some(hook_credential),
                ..TonicDispatcherConfig::default()
            };
            match TonicHookDispatcher::new(&hooks_cfg, dispatcher_cfg) {
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
            custom_policies_dir: custom_dir.clone(),
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
        #[cfg(feature = "mssql")]
        DbParams::Mssql(p) => {
            let backend = schema_forge_mssql::MssqlBackend::connect(&p.config)
                .await
                .map_err(|e| CliError::Server {
                    message: format!("SQL Server connection failed: {e}"),
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
            message: "User system schema is not registered; cannot build EntityAuthStore. \
                 Confirm `seed_system_schemas_into_map` ran during InitForgeData::build."
                .into(),
        })?;

    let policy_store = init_data
        .policy_store
        .clone()
        .ok_or_else(|| CliError::Server {
            message: "policy_store missing from InitForgeData; cannot build EntityAuthStore".into(),
        })?;

    let resolver: schema_forge_backend::entity_auth_store::RoleRankResolver =
        Arc::new(move |role: &str| policy_store.current().role_ranks.get(role));

    let mut store = schema_forge_backend::EntityAuthStore::new(entity_store, user_schema, resolver);
    // Attach the TenantMembership schema when the system seed registered
    // it (which is always for non-legacy deployments). Without it,
    // `list_tenant_memberships` returns an empty `Vec` and the login
    // handler treats the user as unscoped — which is fine for the
    // tenancy-disabled path.
    if let Some(tm_schema) = init_data.registry.get("TenantMembership").cloned() {
        store = store.with_tenant_membership_schema(tm_schema);
    }
    Ok(Arc::new(store))
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
/// Nests SchemaForge's JSON API routes under `/api/v1/forge/`, plus the login
/// endpoint. A build with the `embedded-console` feature additionally serves
/// the bundled ops console same-origin at `/` (mounted as the router fallback
/// by [`mount_console`]); `schemaforge site generate` remains available for a
/// separately-hosted, per-entity React project.
#[allow(clippy::too_many_arguments)]
fn build_versioned_routes(
    auth_store: Arc<dyn schema_forge_acton::DynAuthStore>,
    paseto_generator: Arc<PasetoGenerator>,
    paseto_validator: Arc<PasetoAuth>,
    invite_store: Arc<dyn schema_forge_backend::InviteStore>,
    email_sender: Arc<dyn schema_forge_acton::email::EmailSender>,
    meta_info: Arc<schema_forge_acton::MetaInfo>,
    principal_claims: Arc<schema_forge_acton::authz::PrincipalClaimMappings>,
    tenant_config: Arc<Option<schema_forge_backend::tenant::TenantConfig>>,
    tenant_scope_state: schema_forge_acton::middleware::tenant_scope::TenantScopeState,
) -> acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig> {
    // Cloned into the add_version closure so the login handler can
    // extract them via axum::Extension.
    let auth_store_layer = auth_store;
    let generator_layer = paseto_generator;
    let validator_layer = paseto_validator;
    let invite_store_layer = invite_store;
    let email_sender_layer = email_sender;
    let meta_layer = meta_info;
    let principal_claims_layer = principal_claims;
    let tenant_config_layer = tenant_config;
    VersionedApiBuilder::<schema_forge_acton::SchemaForgeConfig>::with_config()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, move |router| {
            use axum::Extension;
            SchemaForgeExtension::versioned_forge_routes(router)
                // The tenant_scope middleware runs AFTER acton-service's
                // token middleware (which injects Claims) and BEFORE the
                // handlers below. Layer order in axum is reverse: the last
                // `.layer()` runs first on the request, so wire tenant_scope
                // BEFORE the Extensions block to ensure handlers see the
                // mutated Claims.
                .layer(axum::middleware::from_fn_with_state(
                    tenant_scope_state.clone(),
                    schema_forge_acton::middleware::tenant_scope::middleware,
                ))
                .layer(Extension(auth_store_layer))
                .layer(Extension(generator_layer))
                .layer(Extension(validator_layer))
                .layer(Extension(invite_store_layer))
                .layer(Extension(email_sender_layer))
                .layer(Extension(meta_layer))
                .layer(Extension(principal_claims_layer))
                .layer(Extension(tenant_config_layer))
        })
        .build_routes()
}

/// Build a [`PasetoAuth`] validator from the loaded acton-service config.
///
/// Shares the key file the token middleware and [`build_paseto_generator`]
/// use, so a token minted by the generator round-trips through this validator.
/// Used by the invite-accept endpoint to re-verify a stored invite token.
fn build_paseto_validator(
    svc_config: &acton_service::config::Config<schema_forge_acton::SchemaForgeConfig>,
) -> Result<Arc<PasetoAuth>, CliError> {
    let paseto_cfg = match &svc_config.token {
        Some(acton_service::config::TokenConfig::Paseto(pc)) => pc,
        _ => {
            return Err(CliError::Config {
                message: "[token] must be configured with format = \"paseto\" to verify \
                          invitation tokens"
                    .to_string(),
            });
        }
    };
    let validator = PasetoAuth::new(paseto_cfg).map_err(|e| CliError::Config {
        message: format!("failed to build PASETO validator: {e}"),
    })?;
    Ok(Arc::new(validator))
}

/// Layer the SchemaForge `/health` override middleware onto the versioned
/// routes returned by [`build_versioned_routes`].
///
/// This is the implementation of the local workaround for issue #55:
/// `acton-service`'s `/health` handler hard-codes its own
/// `CARGO_PKG_VERSION`, so without this layer `/health` reports the
/// `acton-service` crate version (currently 0.23.x) instead of the running
/// `schema-forge-acton` crate version. The middleware short-circuits
/// `GET /health` and emits a wire-compatible response with the correct
/// version, leaving every other path (notably `/ready` and the nested
/// `/api/v1/...` tree) untouched.
///
/// `VersionedApiBuilder::build_routes()` is documented to always return
/// `WithState`, but we keep the `WithoutState` branch as a defensive
/// pass-through in case upstream changes the contract.
/// Mount the embedded ops console under `/console`, served same-origin by
/// [`crate::commands::serve_console`]. `/console` is registered as a public
/// path (see `run`) so the static SPA loads before the user has a token; the
/// prefix is disjoint from `/api`, which stays bearer-protected. Scoping the
/// console to its own prefix (rather than the router fallback) is what lets a
/// single public-path entry exempt the whole SPA without exposing the API.
///
/// Mirrors [`wrap_health_with_schema_forge_version`]'s `VersionedRoutes`
/// destructure so it composes through the same `ServiceBuilder::with_routes`
/// seam without needing acton-service's htmx-gated `with_frontend_routes`.
#[cfg(feature = "embedded-console")]
fn mount_console(
    routes: acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig>,
) -> acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig> {
    use crate::commands::serve_console::handler;
    use acton_service::service_builder::VersionedRoutes;
    use axum::routing::get;

    // Three routes cover the SPA: `/console` (no slash), `/console/` (root with
    // slash), and `/console/{*rest}` (assets + client-side deep links). The
    // handler strips the `/console` prefix and serves the embedded asset, or
    // `index.html` for an unknown sub-path (SPA history fallback).
    fn add<S>(router: axum::Router<S>) -> axum::Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        router
            .route("/console", get(handler))
            .route("/console/", get(handler))
            .route("/console/{*rest}", get(handler))
    }

    match routes {
        VersionedRoutes::WithState(router) => VersionedRoutes::WithState(add(router)),
        VersionedRoutes::WithoutState(router) => VersionedRoutes::WithoutState(add(router)),
    }
}

fn wrap_health_with_schema_forge_version(
    routes: acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig>,
) -> acton_service::service_builder::VersionedRoutes<schema_forge_acton::SchemaForgeConfig> {
    use acton_service::service_builder::VersionedRoutes;
    match routes {
        VersionedRoutes::WithState(router) => {
            let wrapped = router.layer(axum::middleware::from_fn(
                schema_forge_acton::schema_forge_health_middleware,
            ));
            VersionedRoutes::WithState(wrapped)
        }
        VersionedRoutes::WithoutState(router) => {
            let wrapped = router.layer(axum::middleware::from_fn(
                schema_forge_acton::schema_forge_health_middleware,
            ));
            VersionedRoutes::WithoutState(wrapped)
        }
    }
}

/// Resolve the custom-Cedar-policies directory for the live serve path.
///
/// Precedence:
/// 1. `--custom-dir` on the command line.
/// 2. `[schema_forge.authz] custom_policies_dir` in `config.toml`.
/// 3. `policies/custom` relative to the working directory **iff** that
///    directory exists. Auto-discovery is suppressed when the directory is
///    absent so deployments that don't use the convention don't get a
///    misleading "custom: policies/custom" log line.
///
/// An explicitly-named missing directory is still returned so the caller can
/// warn the operator; `load_custom_policies` already treats it as empty.
fn resolve_custom_policies_dir(
    cli_dir: Option<&Path>,
    config_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(p) = cli_dir {
        return Some(p.to_path_buf());
    }
    if let Some(p) = config_dir {
        return Some(p.to_path_buf());
    }
    let default = PathBuf::from("policies/custom");
    default.is_dir().then_some(default)
}

/// Build a `MetaInfo` snapshot from the resolved DB params.
///
/// `backend` and `backend_label` are picked off the `DbParams` variant so
/// the `/meta` endpoint reports the same backend the rest of the runtime
/// is actually talking to (no separate config knob, no drift).
fn build_meta_info(db_params: &DbParams) -> Arc<schema_forge_acton::MetaInfo> {
    let (backend, label) = match db_params {
        #[cfg(feature = "surrealdb")]
        DbParams::Surrealdb(_) => ("surrealdb", "SurrealDB 2.x"),
        #[cfg(feature = "postgres")]
        DbParams::Postgres(_) => ("postgres", "PostgreSQL"),
        #[cfg(feature = "mssql")]
        DbParams::Mssql(_) => ("mssql", "Microsoft SQL Server"),
        #[allow(unreachable_patterns)]
        _ => ("unknown", "Unknown backend"),
    };
    let ttl = schema_forge_acton::routes::auth::LOGIN_TOKEN_LIFETIME.as_secs();
    Arc::new(schema_forge_acton::MetaInfo::new(backend, label, ttl))
}

/// Backend-agnostic tests for `resolve_custom_policies_dir`. Kept out of the
/// SurrealDB-gated module below so they run on every build.
#[cfg(test)]
mod resolve_tests {
    use super::resolve_custom_policies_dir;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // CWD is process-global. Serialize the two tests below that mutate it
    // so they don't race against each other under `cargo nextest run`.
    static CWD_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn cli_flag_wins_over_config() {
        let cli = PathBuf::from("/cli/custom");
        let cfg = PathBuf::from("/cfg/custom");
        let got = resolve_custom_policies_dir(Some(&cli), Some(&cfg));
        assert_eq!(got.as_deref(), Some(cli.as_path()));
    }

    #[test]
    fn config_used_when_cli_absent() {
        let cfg = PathBuf::from("/cfg/custom");
        let got = resolve_custom_policies_dir(None, Some(&cfg));
        assert_eq!(got.as_deref(), Some(cfg.as_path()));
    }

    #[test]
    fn explicit_missing_dir_is_passed_through_for_warning() {
        // Resolver does not stat the explicit path — `load_custom_policies`
        // treats a missing directory as empty, and the caller emits a warn
        // line. The resolver must return the path so the caller can warn.
        let cli = PathBuf::from("/definitely/does/not/exist");
        let got = resolve_custom_policies_dir(Some(&cli), None);
        assert_eq!(got.as_deref(), Some(cli.as_path()));
    }

    #[test]
    fn default_returns_none_when_policies_custom_missing() {
        // Run from a temp dir with no `policies/custom` subdirectory so
        // the auto-discovery branch returns None instead of a phantom path.
        let _guard = CWD_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(tmp.path()).expect("set cwd");
        let got = resolve_custom_policies_dir(None, None);
        std::env::set_current_dir(prev).expect("restore cwd");
        assert!(
            got.is_none(),
            "auto-discovery should return None when policies/custom does not exist"
        );
    }

    #[test]
    fn default_returns_path_when_policies_custom_exists() {
        let _guard = CWD_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("policies/custom")).expect("mkdir");
        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(tmp.path()).expect("set cwd");
        let got = resolve_custom_policies_dir(None, None);
        std::env::set_current_dir(prev).expect("restore cwd");
        assert_eq!(got, Some(PathBuf::from("policies/custom")));
    }
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

        use std::io::Write as _;

        let key = [0u8; 32];
        let generator = Arc::new(PasetoGenerator::with_symmetric_key(
            key,
            TokenGenerationConfig::default(),
        ));

        // A matching on-disk key so `PasetoAuth::new` can build a validator
        // sharing the generator's symmetric key.
        let mut key_file = tempfile::NamedTempFile::new().unwrap();
        key_file.write_all(&key).unwrap();
        key_file.flush().unwrap();
        let validator = Arc::new(
            PasetoAuth::new(&acton_service::config::PasetoConfig {
                version: "v4".to_string(),
                purpose: "local".to_string(),
                key_path: key_file.path().to_path_buf(),
                issuer: None,
                audience: None,
                public_paths: vec![],
            })
            .unwrap(),
        );

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
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
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
        let auth_store: Arc<dyn schema_forge_acton::DynAuthStore> = Arc::new(EntityAuthStore::new(
            entity_store.clone(),
            user_schema,
            resolver,
        ));

        let invite_store = rt
            .block_on(schema_forge_acton::system::provision_invite_store(
                backend.as_ref(),
                entity_store.clone(),
            ))
            .unwrap();
        let email_sender: Arc<dyn schema_forge_acton::email::EmailSender> =
            Arc::new(schema_forge_acton::email::DisabledEmailSender::new(None));

        let meta = Arc::new(schema_forge_acton::MetaInfo::new(
            "surrealdb",
            "SurrealDB 2.x",
            3600,
        ));
        let principal_claims =
            Arc::new(schema_forge_acton::authz::PrincipalClaimMappings::default());
        let tenant_config = Arc::new(None::<schema_forge_backend::tenant::TenantConfig>);
        let tenant_scope_state = schema_forge_acton::middleware::tenant_scope::TenantScopeState {
            entity_store: entity_store.clone(),
            tenant_config: tenant_config.clone(),
        };
        let _routes = build_versioned_routes(
            auth_store,
            generator,
            validator,
            invite_store,
            email_sender,
            meta,
            principal_claims,
            tenant_config,
            tenant_scope_state,
        );
    }
}
