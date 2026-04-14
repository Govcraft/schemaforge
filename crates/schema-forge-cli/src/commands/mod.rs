pub mod apply;
pub mod codegen;
pub mod completions;
pub mod export;
pub mod hooks;
pub mod init;
pub mod inspect;
pub mod migrate;
pub mod parse;
pub mod policies;
pub mod serve;
pub mod token;

use std::sync::Arc;

use schema_forge_acton::DynForgeBackend;

use crate::config::DbParams;
use crate::error::CliError;
use crate::output::OutputContext;
use crate::progress;

/// Connect to the configured database backend, with fallback to in-memory for SurrealDB.
///
/// Returns a trait object that implements both `SchemaBackend` and `EntityStore`.
/// The concrete backend is selected based on the URL scheme in `db_params`.
pub async fn connect_backend(
    db_params: &DbParams,
    output: &OutputContext,
) -> Result<Arc<dyn DynForgeBackend>, CliError> {
    let spinner = if output.show_progress() {
        Some(progress::create_spinner("Connecting to backend..."))
    } else {
        None
    };

    let result = connect_backend_inner(db_params).await;

    match result {
        Ok(backend) => {
            if let Some(sp) = &spinner {
                progress::finish_spinner(sp, &format!("Connected to {}", db_params.url()));
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

async fn connect_backend_inner(db_params: &DbParams) -> Result<Arc<dyn DynForgeBackend>, CliError> {
    match db_params {
        #[cfg(feature = "surrealdb")]
        DbParams::Surrealdb(p) => {
            let result = schema_forge_surrealdb::SurrealBackend::connect_with_auth(
                &p.url,
                &p.namespace,
                &p.database,
                p.username.as_deref(),
                p.password.as_deref(),
            )
            .await;

            match result {
                Ok(b) => Ok(Arc::new(b)),
                Err(remote_err) => {
                    eprintln!(
                        "Warning: Could not connect to {}; falling back to in-memory backend: {remote_err}",
                        p.url
                    );
                    let b = schema_forge_surrealdb::SurrealBackend::connect_memory(
                        &p.namespace,
                        &p.database,
                    )
                    .await
                    .map_err(CliError::Backend)?;
                    Ok(Arc::new(b))
                }
            }
        }
        #[cfg(feature = "postgres")]
        DbParams::Postgres(p) => {
            let b = schema_forge_postgres::PgBackend::connect(&p.url)
                .await
                .map_err(CliError::Backend)?;
            Ok(Arc::new(b))
        }
        #[allow(unreachable_patterns)]
        other => Err(CliError::Config {
            message: format!(
                "backend '{}' is not enabled in this build (check Cargo features)",
                other.url()
            ),
        }),
    }
}
