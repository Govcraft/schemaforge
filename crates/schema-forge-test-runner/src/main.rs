//! Own one disposable remote database for an entire test command.

use std::{error::Error, process::ExitCode};
use testcontainers::{
    core::{wait::HttpWaitStrategy, IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};
use tokio::process::Command;

#[tokio::main]
async fn main() -> Result<ExitCode, Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let program = arguments
        .next()
        .ok_or("usage: schema-forge-test-runner PROGRAM [ARGS...]")?;
    let container = GenericImage::new("surrealdb/surrealdb", "v3.3.0")
        .with_exposed_port(8000.tcp())
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/health").with_expected_status_code(200u16),
        ))
        .with_cmd([
            "start",
            "--bind",
            "0.0.0.0:8000",
            "--user",
            "root",
            "--pass",
            "schemaforge-test",
            "memory",
        ])
        .start()
        .await?;
    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(8000.tcp()).await?;
    let mut child = Command::new(program)
        .args(arguments)
        .env(
            "SCHEMAFORGE_TEST_SURREALDB_URL",
            format!("ws://{host}:{port}"),
        )
        .env("SCHEMAFORGE_TEST_SURREALDB_USER", "root")
        .env("SCHEMAFORGE_TEST_SURREALDB_PASSWORD", "schemaforge-test")
        .kill_on_drop(true)
        .spawn()?;
    let result = tokio::select! {
        status = child.wait() => status,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            child.kill().await?;
            child.wait().await
        }
    };
    container.rm().await?;
    Ok(if result?.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
