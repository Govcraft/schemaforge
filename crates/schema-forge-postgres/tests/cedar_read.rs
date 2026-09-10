use std::collections::BTreeMap;
use std::sync::Arc;

use schema_forge_backend::auth::CedarReadScope;
use schema_forge_backend::{Entity, EntityStore, SchemaBackend};
use schema_forge_core::migration::DiffEngine;
use schema_forge_core::query::{FieldPath, Filter, Query, SortOrder};
use schema_forge_core::types::{
    Cardinality, DynamicValue, EntityId, EnumVariants, FieldAnnotation, FieldDefinition,
    FieldModifier, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName, TextConstraints,
};
use schema_forge_postgres::PgBackend;
use sqlx::postgres::PgPoolOptions;

fn field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::new(FieldName::new(name).unwrap(), field_type)
}

fn definition() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("CountProof").unwrap(),
        vec![field(
            "label",
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![],
    )
    .unwrap()
}

async fn install(backend: &PgBackend, schema: &SchemaDefinition) {
    backend
        .apply_migration(&schema.name, &DiffEngine::create_new(schema).steps)
        .await
        .unwrap();
    backend.store_schema_metadata(schema).await.unwrap();
}

async fn execute(backend: &PgBackend, sql: &str) {
    sqlx::query(sql).execute(backend.pool()).await.unwrap();
}

async fn insert(backend: &PgBackend, schema: &SchemaDefinition, label: &str) -> Entity {
    backend
        .create(&Entity::new(
            schema.name.clone(),
            BTreeMap::from([("label".into(), DynamicValue::Text(label.into()))]),
        ))
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn exact_tenant_totals_preserve_filter_sort_page_and_count_opt_out() {
    with_database(|backend| async move {
        let schema = definition();
        install(&backend, &schema).await;
        execute(
            &backend,
            "ALTER TABLE \"CountProof\" ADD COLUMN _tenant TEXT",
        )
        .await;
        for (label, tenant) in [
            ("a", None),
            ("b", Some("tenant_one")),
            ("c", Some("tenant_two")),
            ("d", Some("tenant_one")),
        ] {
            let entity = insert(&backend, &schema, label).await;
            sqlx::query("UPDATE \"CountProof\" SET _tenant = $1 WHERE id = $2")
                .bind(tenant)
                .bind(entity.id.as_str())
                .execute(backend.pool())
                .await
                .unwrap();
        }
        let scope = CedarReadScope::TenantMembers(vec!["tenant_one".into()]);
        let mut query = Query::new(schema.id.clone()).with_limit(1).with_offset(1);
        query.sort = vec![(FieldPath::single("label"), SortOrder::Descending)];
        let result = backend
            .query_cedar_compatible(&schema, &query, &scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.total_count, Some(3));
        assert_eq!(
            result.entities[0].fields["label"],
            DynamicValue::Text("b".into())
        );
        query.filter = Some(Filter::ne(
            FieldPath::single("label"),
            DynamicValue::Text("a".into()),
        ));
        let result = backend
            .query_cedar_compatible(&schema, &query, &scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.total_count, Some(2));
        query.include_total = false;
        assert_eq!(
            backend
                .query_cedar_compatible(&schema, &query, &scope)
                .await
                .unwrap()
                .unwrap()
                .total_count,
            None
        );
        query.include_total = true;
        query.offset = Some(99);
        let result = backend
            .query_cedar_compatible(&schema, &query, &scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.total_count, Some(2));
        assert!(result.entities.is_empty());
        let unrestricted = backend
            .query_cedar_compatible(
                &schema,
                &Query::new(schema.id.clone()),
                &CedarReadScope::Unrestricted,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unrestricted.total_count, Some(4));
        let empty_members = backend
            .query_cedar_compatible(
                &schema,
                &Query::new(schema.id.clone()),
                &CedarReadScope::TenantMembers(vec![]),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(empty_members.total_count, Some(1));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn absent_tenant_and_schema_or_physical_drift_are_rechecked() {
    with_database(|backend| async move {
        let schema = definition();
        install(&backend, &schema).await;
        insert(&backend, &schema, "first").await;
        let query = Query::new(schema.id.clone());
        let scope = CedarReadScope::TenantMembers(vec![]);
        assert_eq!(
            backend
                .query_cedar_compatible(&schema, &query, &scope)
                .await
                .unwrap()
                .unwrap()
                .total_count,
            Some(1)
        );
        let mut projected = query.clone();
        projected.projection = Some(vec!["label".into()]);
        assert!(backend
            .query_cedar_compatible(&schema, &projected, &scope)
            .await
            .unwrap()
            .is_none());
        execute(
            &backend,
            "ALTER TABLE \"CountProof\" ADD COLUMN stale_tenant TEXT",
        )
        .await;
        assert!(backend
            .query_cedar_compatible(&schema, &query, &scope)
            .await
            .unwrap()
            .is_none());
        execute(
            &backend,
            "ALTER TABLE \"CountProof\" DROP COLUMN stale_tenant",
        )
        .await;
        execute(
            &backend,
            "ALTER TABLE \"CountProof\" ADD COLUMN _tenant BIGINT",
        )
        .await;
        assert!(backend
            .query_cedar_compatible(&schema, &query, &scope)
            .await
            .unwrap()
            .is_none());
        execute(&backend, "ALTER TABLE \"CountProof\" DROP COLUMN _tenant").await;
        let mut changed = schema.clone();
        changed.fields[0].modifiers.push(FieldModifier::Required);
        backend.store_schema_metadata(&changed).await.unwrap();
        assert!(backend
            .query_cedar_compatible(&schema, &query, &scope)
            .await
            .unwrap()
            .is_none());
        assert!(backend
            .query_cedar_compatible(&changed, &query, &scope)
            .await
            .unwrap()
            .is_some());
        execute(&backend, "UPDATE \"CountProof\" SET label = NULL").await;
        assert!(backend
            .query_cedar_compatible(&changed, &query, &scope)
            .await
            .unwrap()
            .is_none());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn ordinary_rich_types_are_certified_but_unsafe_arrays_dates_and_tenants_fall_back() {
    with_database(|backend| async move {
        let mut schema = definition();
        schema.fields[0].annotations.push(FieldAnnotation::FieldAccess {
            read: vec!["reader".into()], write: vec!["editor".into()],
        });
        schema.fields.extend([
            field("status", FieldType::Enum(EnumVariants::new(vec!["active".into(), "closed".into()]).unwrap())),
            field("tags", FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained())))),
            field("payload", FieldType::Json),
            field("amount", FieldType::Integer(Default::default())),
            field("enabled", FieldType::Boolean),
            field("created", FieldType::DateTime),
            field("parent", FieldType::Relation { target: schema.name.clone(), cardinality: Cardinality::One }),
        ]);
        install(&backend, &schema).await;
        let entity = insert(&backend, &schema, "rich").await;
        sqlx::query("UPDATE \"CountProof\" SET status = 'active', tags = ARRAY['one','two'], payload = '{\"nested\": [1,true]}', amount = 42, enabled = true, created = '2026-01-01 UTC', parent = $1")
            .bind(entity.id.as_str()).execute(backend.pool()).await.unwrap();
        let query = Query::new(schema.id.clone());
        let scope = CedarReadScope::Unrestricted;
        let result = backend.query_cedar_compatible(&schema, &query, &scope).await.unwrap().unwrap();
        assert_eq!(result.total_count, Some(1));
        assert!(matches!(result.entities[0].fields["parent"], DynamicValue::Ref(_)));
        execute(&backend, "UPDATE \"CountProof\" SET tags = ARRAY['one',NULL]").await;
        assert!(backend.query_cedar_compatible(&schema, &query, &scope).await.unwrap().is_none());
        execute(&backend, "UPDATE \"CountProof\" SET tags = ARRAY[['one'],['two']]").await;
        assert!(backend.query_cedar_compatible(&schema, &query, &scope).await.unwrap().is_none());
        execute(&backend, "UPDATE \"CountProof\" SET tags = ARRAY[]::text[], created = 'infinity'").await;
        assert!(backend.query_cedar_compatible(&schema, &query, &scope).await.unwrap().is_none());
        execute(&backend, "UPDATE \"CountProof\" SET created = NULL").await;
        execute(&backend, "ALTER TABLE \"CountProof\" ADD COLUMN _tenant TEXT").await;
        execute(&backend, "UPDATE \"CountProof\" SET _tenant = 'has\"quote'").await;
        assert!(backend.query_cedar_compatible(&schema, &query, &scope).await.unwrap().is_none());
        execute(&backend, "UPDATE \"CountProof\" SET _tenant = NULL").await;
        assert!(backend.query_cedar_compatible(&schema, &query, &scope).await.unwrap().is_some());
    }).await;
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn unselected_json_decoder_errors_do_not_disable_exact_authorized_totals() {
    with_database(|backend| async move {
        let mut schema = definition();
        schema.fields.push(field("payload", FieldType::Json));
        install(&backend, &schema).await;
        insert(&backend, &schema, "a_valid").await;
        let invalid = insert(&backend, &schema, "z_invalid").await;
        let mut query = Query::new(schema.id.clone()).with_limit(1);
        query.sort = vec![(FieldPath::single("label"), SortOrder::Ascending)];
        let scope = CedarReadScope::Unrestricted;
        assert_eq!(
            backend
                .query_cedar_compatible(&schema, &query, &scope)
                .await
                .unwrap()
                .unwrap()
                .total_count,
            Some(2)
        );
        for json in [
            "1e400".to_string(),
            format!("{}0{}", "[".repeat(150), "]".repeat(150)),
        ] {
            sqlx::query("UPDATE \"CountProof\" SET payload = $1::jsonb WHERE id = $2")
                .bind(&json)
                .bind(invalid.id.as_str())
                .execute(backend.pool())
                .await
                .unwrap();
            let rendered: String =
                sqlx::query_scalar("SELECT payload::text FROM \"CountProof\" WHERE id = $1")
                    .bind(invalid.id.as_str())
                    .fetch_one(backend.pool())
                    .await
                    .unwrap();
            assert!(serde_json::from_str::<serde_json::Value>(&rendered).is_err());
            if json == "1e400" {
                assert_eq!(rendered, format!("1{}", "0".repeat(400)));
            }
            // JSON contributes no Cedar attributes. The unselected payload's
            // decoder failure must not prevent an exact authorized count.
            assert_eq!(backend.query(&query).await.unwrap().entities.len(), 1);
            assert!(backend.query(&Query::new(schema.id.clone())).await.is_err());
            let result = backend
                .query_cedar_compatible(&schema, &query, &scope)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.total_count, Some(2));
            assert_eq!(result.entities.len(), 1);
            let selected_invalid = query.clone().with_offset(1);
            assert!(backend
                .query_cedar_compatible(&schema, &selected_invalid, &scope)
                .await
                .is_err());
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn identifier_boundaries_preserve_decoder_compatibility() {
    with_database(|backend| async move {
        let schema = definition();
        install(&backend, &schema).await;
        let entity = insert(&backend, &schema, "id-boundaries").await;
        let suffix = entity.id.as_str().rsplit_once('_').unwrap().1;
        let cases = [
            (format!("a_{suffix}"), true),
            (suffix.to_string(), true),
            (format!("{}_{suffix}", "a".repeat(63)), true),
            (format!("a_b_{suffix}"), true),
            (format!("{}_{suffix}", "a".repeat(64)), false),
            (format!("_a_{suffix}"), false),
            (format!("a__{suffix}"), false),
            (format!("1_{suffix}"), false),
            (format!("a_8{}", &suffix[1..]), false),
            (format!("a_{}i", &suffix[..25]), false),
            (format!("a_{}", &suffix[..25]), false),
            (format!("a_{}0", suffix), false),
            ("".into(), false),
        ];
        let mut previous = entity.id.to_string();
        let query = Query::new(schema.id.clone());
        for (id, accepted) in cases {
            if accepted {
                assert!(
                    EntityId::parse(&id).is_ok(),
                    "accepted SQL shape must decode: {id}"
                );
            }
            sqlx::query("UPDATE \"CountProof\" SET id = $1 WHERE id = $2")
                .bind(&id)
                .bind(&previous)
                .execute(backend.pool())
                .await
                .unwrap();
            let result = backend
                .query_cedar_compatible(&schema, &query, &CedarReadScope::Unrestricted)
                .await
                .unwrap();
            assert_eq!(result.is_some(), accepted, "identifier: {id}");
            previous = id;
        }
    })
    .await;
}

async fn assert_selected_json_is_readable(
    backend: &PgBackend,
    schema: &SchemaDefinition,
    json: &str,
) {
    sqlx::query("UPDATE \"CountProof\" SET payload = $1::jsonb")
        .bind(json)
        .execute(backend.pool())
        .await
        .unwrap();
    let rendered: String = sqlx::query_scalar("SELECT payload::text FROM \"CountProof\"")
        .fetch_one(backend.pool())
        .await
        .unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&rendered).is_ok());
    let query = Query::new(schema.id.clone());
    assert!(backend.query(&query).await.is_ok());
    let result = backend
        .query_cedar_compatible(schema, &query, &CedarReadScope::Unrestricted)
        .await
        .unwrap();
    assert_eq!(result.unwrap().total_count, Some(1));
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn broad_json_and_quoted_delimiters_or_digits_remain_certified() {
    with_database(|backend| async move {
        let mut schema = definition();
        schema.fields.push(field("payload", FieldType::Json));
        install(&backend, &schema).await;
        insert(&backend, &schema, "broad").await;
        let array = (0..300)
            .map(|index| serde_json::json!({"index": index, "nested": [true, null]}))
            .collect::<Vec<_>>();
        let object = (0..300)
            .map(|index| {
                (
                    format!("field_{index}"),
                    serde_json::json!({"value": index}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        for json in [
            serde_json::json!(array),
            serde_json::Value::Object(object),
            serde_json::json!({"delimiters": "[{}]".repeat(500), "digits": "9".repeat(600),
                "numeric_text": "1e400", "quoted": "\\\"[".repeat(100)}),
        ] {
            assert_selected_json_is_readable(&backend, &schema, &json.to_string()).await;
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires SCHEMAFORGE_TEST_POSTGRES_URL with CREATE SCHEMA privilege"]
async fn required_and_hidden_json_cannot_change_tenant_visibility() {
    with_database(|backend| async move {
        let mut schema = definition();
        let mut required = field("payload", FieldType::Json);
        required.modifiers.push(FieldModifier::Required);
        let mut hidden = field("secret", FieldType::Json);
        hidden.annotations.push(FieldAnnotation::Hidden);
        schema.fields.extend([required, hidden]);
        install(&backend, &schema).await;
        execute(&backend, "ALTER TABLE \"CountProof\" ADD COLUMN _tenant TEXT").await;
        for label in ["a_valid", "z_invalid"] {
            backend.create(&Entity::new(schema.name.clone(), BTreeMap::from([
                ("label".into(), DynamicValue::Text(label.into())),
                ("payload".into(), DynamicValue::Json(serde_json::json!({}))),
                ("secret".into(), DynamicValue::Json(serde_json::json!({}))),
            ]))).await.unwrap();
        }
        sqlx::query("UPDATE \"CountProof\" SET payload = '1e400', secret = $1::jsonb, _tenant = 'foreign' WHERE label = 'z_invalid'")
            .bind(format!("{}0{}", "[".repeat(150), "]".repeat(150)))
            .execute(backend.pool()).await.unwrap();
        let mut query = Query::new(schema.id.clone()).with_limit(1);
        query.sort = vec![(FieldPath::single("label"), SortOrder::Ascending)];
        let result = backend.query_cedar_compatible(&schema, &query, &CedarReadScope::Unrestricted)
            .await.unwrap().unwrap();
        assert_eq!(result.total_count, Some(2));
        let tenant_scope = CedarReadScope::TenantMembers(vec!["own".into()]);
        let result = backend.query_cedar_compatible(&schema, &query, &tenant_scope)
            .await.unwrap().unwrap();
        assert_eq!(result.total_count, Some(1));
        assert_eq!(result.entities[0].fields["label"], DynamicValue::Text("a_valid".into()));
        assert!(backend.query_cedar_compatible(&schema, &query.with_offset(1), &CedarReadScope::Unrestricted)
            .await.is_err());
    }).await;
}

async fn with_database<Test, Pending>(test: Test)
where
    Test: FnOnce(Arc<PgBackend>) -> Pending,
    Pending: std::future::Future<Output = ()> + Send + 'static,
{
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL").expect("test PostgreSQL URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let namespace = EntityId::new("countproof").to_string();
    sqlx::query(&format!("CREATE SCHEMA \"{namespace}\""))
        .execute(&admin)
        .await
        .unwrap();
    let scope = namespace.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let scope = scope.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path', $1, false)")
                    .bind(scope)
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let backend = Arc::new(PgBackend::from_pool(pool.clone()).await.unwrap());
    let result = tokio::spawn(test(backend)).await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
