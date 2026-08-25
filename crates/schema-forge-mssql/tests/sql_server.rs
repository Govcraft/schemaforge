use schema_forge_backend::SchemaBackend;
use schema_forge_mssql::MssqlBackend;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::SyncRunner;
use testcontainers::{GenericImage, ImageExt};

const SA_PASSWORD: &str = "SchemaForge_test_2026!";

#[tokio::test]
#[ignore = "requires Docker and acceptance of the SQL Server container EULA"]
async fn connects_and_initializes_metadata_with_testcontainers() {
    let container = GenericImage::new("mcr.microsoft.com/mssql/server", "2022-latest")
        .with_exposed_port(1433.tcp())
        .with_wait_for(WaitFor::message_on_stdout(
            "SQL Server is now ready for client connections",
        ))
        .with_env_var("ACCEPT_EULA", "Y")
        .with_env_var("MSSQL_SA_PASSWORD", SA_PASSWORD)
        .start()
        .expect("start SQL Server container");
    let port = container
        .get_host_port_ipv4(1433.tcp())
        .expect("mapped SQL Server port");
    let config = toml::from_str(&format!(
        "url = 'Server=127.0.0.1,{port};User Id=sa;Password={SA_PASSWORD};\
         TrustServerCertificate=true'"
    ))
    .expect("valid database config");

    let backend = MssqlBackend::connect(&config)
        .await
        .expect("connect to SQL Server");
    assert!(backend
        .list_schema_metadata()
        .await
        .expect("list initialized metadata")
        .is_empty());
}
