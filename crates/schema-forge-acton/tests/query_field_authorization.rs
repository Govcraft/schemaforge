//! Storage predicates must not reveal fields removed by response projection.
mod support;

use std::{collections::BTreeMap, sync::Arc};

use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Extension, Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    authz::{PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks},
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    storage::StorageRegistry,
    ForgeActor,
};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::DynamicValue;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower::ServiceExt;

async fn fixture(role: &str, custom_policy: &str) -> Router {
    let schemas = schema_forge_dsl::parse(
        r#"
        @export(formats: [csv], bundle_files: false, max_rows: 100)
        @access(read: ["member", "manager"])
        schema Note {
            title: text required @exportable
            restricted: text @field_access(read: ["manager"])
            write_only_rule: text @field_access(write: ["manager"])
            hidden: text @hidden
            parent: -> Note
        }
        @access(read: ["member"])
        schema Other { secret: text @field_access(read: ["manager"]) }
        "#,
    )
    .unwrap();
    let schema = &schemas[0];
    let record = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Public".into())),
            (
                "restricted".into(),
                DynamicValue::Text("Restricted secret".into()),
            ),
            ("hidden".into(), DynamicValue::Text("Hidden secret".into())),
            (
                "write_only_rule".into(),
                DynamicValue::Text("Readable".into()),
            ),
        ]),
    );
    let backend = Arc::new(support::FixtureBackend::new(schema.clone(), vec![record]));
    let policies = tempfile::tempdir().unwrap();
    let custom_policy = format!(
        "{custom_policy}\npermit(principal, action == Action::\"ExportNote\", resource is Note);"
    );
    std::fs::write(policies.path().join("custom.cedar"), custom_policy).unwrap();
    let store = Arc::new(PolicyStore::new(
        PolicyStoreSnapshot::from_schemas(
            &schemas,
            Some(policies.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap(),
    ));
    let service = ServiceBuilder::new()
        .with_config(Config::<SchemaForgeConfig>::default())
        .with_actor::<ForgeActor>()
        .build();
    let (tx, rx) = oneshot::channel();
    service
        .state()
        .actor::<ForgeActor>()
        .unwrap()
        .send(InitForge {
            registry: schemas
                .into_iter()
                .map(|schema| (schema.name.to_string(), schema))
                .collect(),
            backend,
            tenant_config: None,
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: StorageRegistry::default(),
            policy_store: Some(store),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    rx.await.unwrap();
    forge_routes()
        .layer(Extension(Claims {
            sub: "user:reader".into(),
            roles: vec![role.into()],
            perms: vec![],
            exp: 9_999_999_999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            email: None,
            username: None,
            custom: Default::default(),
        }))
        .with_state(service.state().clone())
}

async fn request(app: &Router, query: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (path, method, body) = match body {
        Some(body) => (
            "/schemas/Note/entities/query".to_owned(),
            Method::POST,
            Body::from(body.to_string()),
        ),
        None => (
            format!("/schemas/Note/entities{query}"),
            Method::GET,
            Body::empty(),
        ),
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn assert_denied(app: &Router, query: &str, body: Option<Value>) {
    let (status, response) = request(app, query, body.clone()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{query}, {body:?}: {response}"
    );
    assert!(response
        .to_string()
        .contains("Not authorized to filter this field."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_and_post_predicates_cannot_probe_redacted_fields() {
    let app = fixture("member", "").await;
    let (status, response) = request(&app, "?resolve=false", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["total_count"], 1);
    let fields = &response["entities"][0]["fields"];
    assert_eq!(fields["title"], "Public");
    assert!(fields.get("restricted").is_none());
    assert!(fields.get("hidden").is_none());

    for field in ["restricted", "hidden"] {
        for query in [
            format!("?{field}__eq=secret"),
            format!("?{field}__startswith=s"),
            format!("?sort={field}"),
            format!("?sort=-{field}&count=false&limit=0"),
        ] {
            assert_denied(&app, &query, None).await;
        }
        for filter in [
            json!({"op":"eq", "field":field, "value":"secret"}),
            json!({"op":"startswith", "field":field, "value":"s"}),
            json!({"op":"and", "filters":[
                {"op":"eq", "field":"title", "value":"Public"},
                {"op":"or", "filters":[{"op":"not", "filter":
                    {"op":"eq", "field":field, "value":"secret"}
                }]}
            ]}),
        ] {
            assert_denied(&app, "", Some(json!({"filter":filter}))).await;
        }
        assert_denied(
            &app,
            "",
            Some(json!({"sort":[{"field":field,"order":"desc"}],"count":false,"limit":0})),
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_and_related_paths_fail_closed_even_for_platform_admin() {
    for role in ["member", "manager", "platform_admin"] {
        let app = fixture(role, "").await;
        for field in [
            "hidden",
            "parent.hidden",
            "parent.restricted",
            "parent.title",
        ] {
            assert_denied(&app, &format!("?{field}__eq=secret"), None).await;
            assert_denied(&app, &format!("?sort={field}"), None).await;
            assert_denied(
                &app,
                "",
                Some(json!({"filter":{"op":"eq","field":field,"value":"secret"}})),
            )
            .await;
            assert_denied(&app, "", Some(json!({"sort":[{"field":field}]}))).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readable_root_fields_keep_query_access_without_scanning_storage() {
    // This fixture deliberately rejects storage filters/sorts. A zero page with
    // count=false proves authorization succeeds without an eager full-table scan.
    let unrelated = r#"
        permit(principal, action == Action::"ReadFieldOther_secret", resource is Other);
        forbid(principal, action == Action::"WriteFieldNote_write_only_rule", resource is Note);
    "#;
    for role in ["member", "manager", "platform_admin"] {
        let app = fixture(role, unrelated).await;
        let mut fields = vec!["title", "id", "parent", "write_only_rule"];
        if role != "member" {
            fields.push("restricted");
        }
        for field in fields {
            let value = "note_00000000000000000000000000";
            let query = format!("?{field}__eq={value}&sort={field}&count=false&limit=0");
            let (status, response) = request(&app, &query, None).await;
            assert_eq!(status, StatusCode::OK, "{role}/{field}: {response}");
            let (status, response) = request(
                &app,
                "",
                Some(json!({
                    "filter":{"op":"eq","field":field,"value":value},
                    "sort":[{"field":field}],"limit":0,"count":false
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{role}/{field}: {response}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placeholder_allow_cannot_certify_a_record_dependent_field_policy() {
    let app = fixture(
        "manager",
        r#"
        forbid(principal, action == Action::"ReadFieldNote_restricted", resource is Note)
        when { !context.resource_is_placeholder };
        "#,
    )
    .await;
    let (status, response) = request(&app, "?resolve=false", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(response["entities"][0]["fields"]
        .get("restricted")
        .is_none());
    for query in [
        "?restricted__eq=s",
        "?restricted__startswith=s&count=false&limit=0",
        "?sort=restricted",
    ] {
        assert_denied(&app, query, None).await;
    }
    assert_denied(
        &app,
        "",
        Some(json!({"filter":{"op":"eq","field":"restricted","value":"s"}})),
    )
    .await;
    assert_denied(
        &app,
        "",
        Some(json!({"sort":[{"field":"restricted"}],"limit":0,"count":false})),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_sort_fields_are_client_errors_before_storage() {
    let app = fixture("member", "").await;
    for body in [None, Some(json!({"sort":[{"field":"created_at"}]}))] {
        let (status, response) = request(&app, "?sort=created_at", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert!(response.to_string().contains("created_at"));
        assert!(response.to_string().contains("invalid_query"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_and_async_export_filters_cannot_probe_redacted_fields() {
    let app = fixture("member", "").await;
    for asynchronous in [false, true] {
        for field in ["restricted", "hidden", "parent.restricted"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/schemas/Note/entities/export")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "format":"csv", "async":asynchronous,
                                "filter":{"op":"startswith", "field":field, "value":"s"}
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{field}, async={asynchronous}"
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert!(String::from_utf8_lossy(&body).contains("Not authorized to filter this field."));
        }
    }
}
