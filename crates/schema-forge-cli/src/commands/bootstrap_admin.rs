//! `schemaforge bootstrap-admin` — seed the initial `platform_admin` user.
//!
//! This subcommand is the out-of-band counterpart to the implicit
//! bootstrap path inside `schemaforge serve`. It exists so provisioning
//! pipelines (init containers, ansible/terraform, ad-hoc DR) can create
//! the first platform_admin without starting the HTTP server.
//!
//! Idempotency: the underlying [`schema_forge_acton::shared_auth::bootstrap_admin`]
//! refuses to act when the user store is non-empty, so re-runs are
//! no-ops once a single user (admin or otherwise) exists. That keeps a
//! restart of a Kubernetes init container from creating duplicate
//! admins or rotating credentials silently.
//!
//! Identity store: every user lives in the `User` entity table managed
//! by [`schema_forge_backend::EntityAuthStore`]. The bootstrap path
//! mirrors the canonical `serve` wiring — schemas are loaded, system
//! schemas seeded, the policy store compiled, then the auth store is
//! built on top of the live `User` schema definition. This keeps the
//! out-of-band bootstrap and the running server using the exact same
//! identity backend.

use std::sync::Arc;

use schema_forge_acton::{DynAuthStore, DynForgeBackend, SchemaForgeExtension};

use crate::cli::{BootstrapAdminArgs, GlobalOpts};
use crate::config::{load_svc_config, resolve_db_params, DbParams};
use crate::error::CliError;
use crate::output::OutputContext;

/// Run the `bootstrap-admin` subcommand.
pub async fn run(
    args: BootstrapAdminArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    if args.password.trim().is_empty() {
        return Err(CliError::Config {
            message: "--password (or SCHEMA_FORGE_BOOTSTRAP_ADMIN_PASSWORD) must be a non-empty value"
                .into(),
        });
    }

    let svc_config = load_svc_config(global)?;
    let db_params = resolve_db_params(&svc_config)?;

    output.status(&format!("Connecting to backend at {}…", db_params.url()));
    let connected = connect(&db_params).await?;

    let role_ranks = schema_forge_acton::authz::RoleRanks::from_toml_file(&args.role_ranks)
        .map_err(|e| CliError::Server {
            message: format!(
                "failed to load role ranks from {}: {e}",
                args.role_ranks.display()
            ),
        })?;

    let principal_claims = schema_forge_acton::authz::PrincipalClaimMappings::from_config(
        &svc_config.custom.schema_forge.authz.principal_claims,
    )
    .map_err(|e| CliError::Server {
        message: format!("invalid [schema_forge.authz.principal_claims]: {e}"),
    })?;

    output.status("Loading schemas and policy store…");
    let init_data = SchemaForgeExtension::build_init(
        connected.backend.clone(),
        None,
        &svc_config.custom.schema_forge.storage,
        role_ranks,
        principal_claims,
    )
    .await
    .map_err(|e| CliError::Server {
        message: format!("failed to build init data: {e}"),
    })?;

    // Phase-2 principal-claim validation: bind every `source.user_field`
    // declaration to the loaded User schema. Bootstrap doesn't serve requests
    // and therefore never exercises the IN-side projection, but a
    // misconfigured deployment should still fail fast on bootstrap so
    // operators don't ship broken configs to production.
    let mut resolved_principal_claims = (*init_data.principal_claims).clone();
    let user_schema = init_data
        .registry
        .get("User")
        .ok_or_else(|| CliError::Server {
            message: "User schema is not registered; cannot validate principal-claim sources"
                .to_string(),
        })?;
    resolved_principal_claims
        .resolve_user_field_sources(user_schema)
        .map_err(|e| CliError::Server {
            message: format!("invalid [schema_forge.authz.principal_claims] source binding: {e}"),
        })?;

    let auth_store = build_auth_store(&init_data, connected.entity_store)?;

    schema_forge_acton::shared_auth::bootstrap_admin_with_display_name(
        auth_store.as_ref(),
        &args.username,
        &args.password,
        &args.display_name,
    )
    .await
    .map_err(|e| CliError::Server { message: e })?;

    let count = auth_store
        .count_users()
        .await
        .map_err(|e| CliError::Server {
            message: format!("post-bootstrap count_users failed: {e}"),
        })?;

    if count == 1 {
        output.success(&format!(
            "platform_admin '{}' bootstrapped against {}.",
            args.username,
            db_params.url(),
        ));
    } else {
        output.warn(&format!(
            "User store already has {count} user(s); bootstrap skipped (idempotent)."
        ));
    }

    Ok(())
}

/// Connect-and-erase result: a [`DynForgeBackend`] for schema/entity
/// operations and the [`DynEntityStore`] handle the auth store needs.
struct ConnectedHandles {
    backend: Arc<dyn DynForgeBackend>,
    entity_store: Arc<dyn schema_forge_backend::DynEntityStore>,
}

/// Connect to the configured backend.
async fn connect(db_params: &DbParams) -> Result<ConnectedHandles, CliError> {
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
            Ok(ConnectedHandles {
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
            Ok(ConnectedHandles {
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

fn build_auth_store(
    init_data: &schema_forge_acton::InitForgeData,
    entity_store: Arc<dyn schema_forge_backend::DynEntityStore>,
) -> Result<Arc<dyn DynAuthStore>, CliError> {
    let user_schema = init_data
        .registry
        .get("User")
        .cloned()
        .ok_or_else(|| CliError::Server {
            message: "User system schema is not registered; cannot build EntityAuthStore".into(),
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
