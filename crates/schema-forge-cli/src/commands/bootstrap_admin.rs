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

use std::sync::Arc;

use schema_forge_acton::DynAuthStore;

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
    let auth_store = connect_auth_store(&db_params).await?;

    schema_forge_acton::shared_auth::bootstrap_admin_with_display_name(
        auth_store.as_ref(),
        &args.username,
        &args.password,
        &args.display_name,
    )
    .await
    .map_err(|e| CliError::Server { message: e })?;

    // The bootstrap fn returns Ok when the store already has users — make
    // the result observable so operators know whether the seed actually
    // landed or was a no-op.
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

/// Connect to the configured backend and return its auth store handle.
///
/// Mirrors the connect logic in `serve::connect_once` but without the
/// retry / backend-handle plumbing — bootstrap-admin only needs the
/// auth store interface.
async fn connect_auth_store(db_params: &DbParams) -> Result<Arc<dyn DynAuthStore>, CliError> {
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
            Ok(Arc::new(backend))
        }
        #[cfg(feature = "postgres")]
        DbParams::Postgres(p) => {
            let backend = schema_forge_postgres::PgBackend::connect(&p.url)
                .await
                .map_err(|e| CliError::Server {
                    message: format!("PostgreSQL connection failed: {e}"),
                })?;
            Ok(Arc::new(backend))
        }
        #[allow(unreachable_patterns)]
        other => Err(CliError::Config {
            message: format!("backend '{}' is not enabled in this build", other.url()),
        }),
    }
}
