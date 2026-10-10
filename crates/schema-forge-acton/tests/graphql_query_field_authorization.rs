//! GraphQL predicates must not infer values omitted by read projection.
#![cfg(feature = "graphql")]

mod support;

use std::{collections::BTreeMap, sync::Arc};

use acton_service::{config::Config, middleware::Claims, service_builder::ServiceBuilder};
use async_graphql::{dynamic::Schema, Request, Response};
use schema_forge_acton::{
    authz::{PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks},
    config::SchemaForgeConfig,
    graphql::{context::ForgeGraphqlContext, schema_builder::build_graphql_schema},
    state::{ForgeState, SchemaRegistry},
    storage::StorageRegistry,
};
use schema_forge_backend::Entity;
use schema_forge_core::types::DynamicValue;
use serde_json::{json, Value};

async fn fixture(role: &str, custom_policy: &str) -> (Schema, ForgeGraphqlContext) {
    let schemas = schema_forge_dsl::parse(
        r#"
        @access(read: ["member", "manager"])
        schema Note {
            title: text required
            restricted: text @field_access(read: ["manager"])
            write_only_rule: text @field_access(write: ["manager"])
            hidden: text @hidden
        }
        "#,
    )
    .unwrap();
    let schema = &schemas[0];
    let record = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Public".into())),
            ("restricted".into(), DynamicValue::Text("Secret".into())),
            ("hidden".into(), DynamicValue::Text("Hidden".into())),
        ]),
    );
    let registry = SchemaRegistry::new();
    registry
        .insert(schema.name.to_string(), schema.clone())
        .await;
    let policies = tempfile::tempdir().unwrap();
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
        .build();
    (
        build_graphql_schema(&schemas).unwrap(),
        ForgeGraphqlContext {
            state: ForgeState {
                registry,
                backend: Arc::new(support::FixtureBackend::new(schema.clone(), vec![record])),
                tenant_config: None,
                record_access_policy: None,
                policy_store: store,
                graphql_schema: schema_forge_acton::graphql::empty_graphql_schema(),
                auth_store: None,
                webhook_dispatcher: None,
                storage_registry: StorageRegistry::default(),
            },
            app_state: service.state().clone(),
            claims: Some(Claims {
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
            }),
        },
    )
}

async fn query(role: &str, policy: &str, query: &str) -> Response {
    let (schema, context) = fixture(role, policy).await;
    schema.execute(Request::new(query).data(context)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graphql_filters_and_sorts_refuse_redacted_fields_before_storage() {
    let baseline = query("member", "", "{ notes { items { title restricted } } }").await;
    assert!(baseline.errors.is_empty(), "{:?}", baseline.errors);
    assert_eq!(
        baseline.data.into_json().unwrap()["notes"]["items"][0],
        json!({"title":"Public", "restricted":null})
    );
    for field in ["restricted", "hidden"] {
        for arguments in [
            format!(r#"filter: {{{field}_eq: "Secret"}}"#),
            format!(r#"filter: {{{field}_starts_with: "S"}}"#),
            format!(
                r#"filter: {{and: [{{title_eq: "Public"}}, {{or: [{{not: {{{field}_eq: "Secret"}}}}]}}]}}"#
            ),
            format!("sort: [{{field: {field}, order: DESC}}]"),
        ] {
            let response = query(
                "member",
                "",
                &format!("{{ notes({arguments}) {{ items {{ title }} }} }}"),
            )
            .await;
            let response = serde_json::to_value(response).unwrap();
            assert_eq!(
                response["errors"][0]["extensions"]["code"], "FORBIDDEN",
                "{arguments}: {response}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graphql_readable_predicates_reach_storage_and_hidden_fields_never_do() {
    // The deliberately limited backend rejects all storage predicates. An
    // INTERNAL_ERROR therefore proves the authorized request reached storage.
    for (role, field, code) in [
        ("manager", "restricted", "INTERNAL_ERROR"),
        ("member", "write_only_rule", "INTERNAL_ERROR"),
        ("platform_admin", "hidden", "FORBIDDEN"),
    ] {
        let response = query(
            role,
            "",
            &format!("{{ notes(sort: [{{field: {field}}}]) {{ count }} }}"),
        )
        .await;
        let response: Value = serde_json::to_value(response).unwrap();
        assert_eq!(
            response["errors"][0]["extensions"]["code"], code,
            "{role}, {field}: {response}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graphql_placeholder_allow_cannot_authorize_record_dependent_predicates() {
    let policy = r#"
        forbid(principal, action == Action::"ReadFieldNote_restricted", resource is Note)
        when { !context.resource_is_placeholder && resource has restricted && resource.restricted == "Secret" };
    "#;
    let response = query(
        "manager",
        policy,
        r#"{ notes(filter: {restricted_eq: "Secret"}) { count } }"#,
    )
    .await;
    let response = serde_json::to_value(response).unwrap();
    assert_eq!(
        response["errors"][0]["extensions"]["code"], "FORBIDDEN",
        "{response}"
    );
}
