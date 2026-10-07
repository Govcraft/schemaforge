use std::sync::Arc;

use schema_forge_acton::DynForgeBackend;

use crate::config::DbParams;
use crate::error::CliError;
use crate::output::OutputContext;
use crate::progress;

/// Connect to the configured database backend, failing if it is unavailable.
///
/// Returns a trait object that implements both `SchemaBackend` and `EntityStore`.
/// The concrete backend is selected based on the URL scheme in `db_params`.
pub async fn connect_backend(
    db_params: &DbParams,
    output: &OutputContext,
) -> Result<Arc<dyn DynForgeBackend>, CliError> {
    connect_backend_with_mode(db_params, output, false).await
}

/// Connect without PostgreSQL bookkeeping DDL for inspection and planning.
pub async fn connect_backend_read_only(
    db_params: &DbParams,
    output: &OutputContext,
) -> Result<Arc<dyn DynForgeBackend>, CliError> {
    connect_backend_with_mode(db_params, output, true).await
}

async fn connect_backend_with_mode(
    db_params: &DbParams,
    output: &OutputContext,
    read_only: bool,
) -> Result<Arc<dyn DynForgeBackend>, CliError> {
    let spinner = if output.show_progress() {
        Some(progress::create_spinner("Connecting to backend..."))
    } else {
        None
    };

    let result = connect_backend_inner(db_params, read_only).await;

    match result {
        Ok(backend) => {
            if let Some(sp) = &spinner {
                progress::finish_spinner(sp, &format!("Connected to {}", db_params.redacted_url()));
            }
            Ok(backend)
        }
        Err(e) => {
            if let Some(sp) = &spinner {
                progress::finish_spinner_error(sp, &format!("Connection failed: {e}"));
            }
            Err(e)
        }
    }
}

async fn connect_backend_inner(
    db_params: &DbParams,
    read_only: bool,
) -> Result<Arc<dyn DynForgeBackend>, CliError> {
    // Other backends do not bootstrap PostgreSQL bookkeeping tables.
    let _ = read_only;
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
            .map_err(CliError::Backend)?;
            Ok(Arc::new(backend))
        }
        #[cfg(feature = "postgres")]
        DbParams::Postgres(p) => {
            let b = if read_only {
                schema_forge_postgres::PgBackend::connect_read_only(&p.url).await
            } else {
                schema_forge_postgres::PgBackend::connect(&p.url).await
            }
            .map_err(CliError::Backend)?;
            Ok(Arc::new(b))
        }
        #[cfg(feature = "mssql")]
        DbParams::Mssql(p) => {
            let backend = schema_forge_mssql::MssqlBackend::connect(&p.config)
                .await
                .map_err(CliError::Backend)?;
            Ok(Arc::new(backend))
        }
        #[allow(unreachable_patterns)]
        other => Err(CliError::Config {
            message: format!(
                "backend '{}' is not enabled in this build (check Cargo features)",
                other.redacted_url()
            ),
        }),
    }
}
