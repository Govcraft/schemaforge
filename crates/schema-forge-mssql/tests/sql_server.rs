use std::collections::BTreeMap;

use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::migration::{DiffEngine, MigrationStep};
use schema_forge_core::query::{AggregateOp, AggregateQuery, FieldPath, Filter, Query, SortOrder};
use schema_forge_core::types::{
    DynamicValue, FieldDefinition, FieldName, FieldType, IntegerConstraints, SchemaDefinition,
    SchemaId, SchemaName, TextConstraints,
};
use schema_forge_mssql::MssqlBackend;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::SyncRunner;
use testcontainers::{GenericImage, ImageExt};

const SA_PASSWORD: &str = "SchemaForge_test_2026!";

#[tokio::test]
#[ignore = "requires Docker and acceptance of the SQL Server container EULA"]
async fn connects_and_initializes_metadata_on_sql_server_2019() {
    connects_and_initializes_metadata("2019-latest").await;
}

#[tokio::test]
#[ignore = "requires Docker and acceptance of the SQL Server container EULA"]
async fn connects_and_initializes_metadata_on_sql_server_2022() {
    connects_and_initializes_metadata("2022-latest").await;
}

async fn connects_and_initializes_metadata(image_tag: &str) {
    let container = GenericImage::new("mcr.microsoft.com/mssql/server", image_tag)
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

    exercises_backend_contract(&backend).await;
}

async fn exercises_backend_contract(backend: &MssqlBackend) {
    let schema = product_schema();
    let plan = DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .expect("create product table");
    backend
        .store_schema_metadata(&schema)
        .await
        .expect("store product schema metadata");
    assert_eq!(
        backend
            .load_schema_metadata(&schema.name)
            .await
            .expect("load product schema metadata"),
        Some(schema.clone())
    );

    let alpha = product(&schema.name, "Alpha", 10, true);
    let mut bravo = product(&schema.name, "Bravo", 20, true);
    let charlie = product(&schema.name, "Charlie", 30, false);
    for entity in [&alpha, &bravo, &charlie] {
        assert_eq!(
            backend.create(entity).await.expect("create product"),
            *entity
        );
    }
    assert_eq!(
        backend
            .get(&schema.name, &alpha.id)
            .await
            .expect("get product"),
        alpha
    );

    bravo
        .fields
        .insert("price".into(), DynamicValue::Integer(25));
    backend.update(&bravo).await.expect("update product");
    assert_eq!(
        backend
            .get(&schema.name, &bravo.id)
            .await
            .expect("get updated product")
            .field("price"),
        Some(&DynamicValue::Integer(25))
    );

    let active_filter = Filter::eq(FieldPath::single("active"), DynamicValue::Boolean(true));
    let page = backend
        .query(
            &Query::new(schema.id.clone())
                .with_filter(active_filter.clone())
                .with_sort(FieldPath::single("price"), SortOrder::Descending)
                .with_limit(1)
                .with_offset(1)
                .with_projection(vec!["name".into()]),
        )
        .await
        .expect("query products");
    assert_eq!(page.total_count, Some(2));
    assert_eq!(page.entities.len(), 1);
    assert_eq!(
        page.entities[0].field("name"),
        Some(&DynamicValue::Text("Alpha".into()))
    );
    assert_eq!(page.entities[0].field_count(), 1);
    assert_eq!(
        backend
            .count(&Query::new(schema.id.clone()).with_filter(active_filter.clone()))
            .await
            .expect("count active products"),
        2
    );

    let aggregates = backend
        .aggregate(
            &AggregateQuery::new(schema.id.clone())
                .with_filter(active_filter)
                .with_ops(vec![
                    AggregateOp::Count,
                    AggregateOp::Sum {
                        field: FieldPath::single("price"),
                    },
                    AggregateOp::Avg {
                        field: FieldPath::single("price"),
                    },
                ]),
        )
        .await
        .expect("aggregate active products");
    assert_eq!(
        aggregates
            .iter()
            .map(|result| result.value)
            .collect::<Vec<_>>(),
        vec![2.0, 35.0, 17.5]
    );

    backend
        .delete(&schema.name, &charlie.id)
        .await
        .expect("delete product");
    assert!(backend.get(&schema.name, &charlie.id).await.is_err());
    backend
        .apply_migration(
            &schema.name,
            &[MigrationStep::DropSchema {
                name: schema.name.clone(),
            }],
        )
        .await
        .expect("drop product table");
}

fn product_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Product").expect("valid schema name"),
        vec![
            FieldDefinition::new(
                FieldName::new("name").expect("valid field name"),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("price").expect("valid field name"),
                FieldType::Integer(IntegerConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("active").expect("valid field name"),
                FieldType::Boolean,
            ),
        ],
        vec![],
    )
    .expect("valid product schema")
}

fn product(schema: &SchemaName, name: &str, price: i64, active: bool) -> Entity {
    Entity::new(
        schema.clone(),
        BTreeMap::from([
            ("name".into(), DynamicValue::Text(name.into())),
            ("price".into(), DynamicValue::Integer(price)),
            ("active".into(), DynamicValue::Boolean(active)),
        ]),
    )
}
