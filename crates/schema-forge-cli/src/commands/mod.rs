pub mod apply;
pub mod completions;
pub mod export;
pub mod generate;
pub mod init;
pub mod inspect;
pub mod migrate;
pub mod parse;
pub mod policies;
pub mod serve;

use schema_forge_surrealdb::SurrealBackend;

use crate::config::DbParams;
use crate::error::CliError;
use crate::output::OutputContext;
use crate::progress;

/// Connect to SurrealDB using resolved params, with fallback to in-memory.
pub async fn connect_backend(
    db_params: &DbParams,
    output: &OutputContext,
) -> Result<SurrealBackend, CliError> {
    let spinner = if output.show_progress() {
        Some(progress::create_spinner("Connecting to backend..."))
    } else {
        None
    };

    let result = SurrealBackend::connect_with_auth(
        &db_params.url,
        &db_params.namespace,
        &db_params.database,
        db_params.username.as_deref(),
        db_params.password.as_deref(),
    )
    .await;

    match result {
        Ok(b) => {
            if let Some(sp) = &spinner {
                progress::finish_spinner(sp, &format!("Connected to {}", db_params.url));
            }
            Ok(b)
        }
        Err(remote_err) => {
            if let Some(sp) = &spinner {
                progress::finish_spinner_error(
                    sp,
                    &format!("Remote connection failed: {remote_err}"),
                );
            }
            output.warn(&format!(
                "Could not connect to {}; falling back to in-memory backend.",
                db_params.url
            ));
            SurrealBackend::connect_memory(&db_params.namespace, &db_params.database)
                .await
                .map_err(CliError::Backend)
        }
    }
}
