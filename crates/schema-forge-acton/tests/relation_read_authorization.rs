//! Related labels and inverse IDs obey the same read authority as direct reads.
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use acton_service::{config::Config, middleware::Claims, prelude::ActorHandleInterface};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    ForgeActor,
};
use schema_forge_backend::{auth::RecordAccessPolicy, entity::Entity, SchemaBackend};
use schema_forge_core::types::{DynamicValue, EntityId, FieldName, SchemaDefinition};
use schema_forge_surrealdb::SurrealBackend;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower::ServiceExt;

fn fixture_id(prefix: &str) -> EntityId {
    EntityId::parse(&format!("{prefix}_00000000000000000000000000")).unwrap()
}

struct OperatorPolicy {
    redact_authorization_attribute: bool,
}
impl RecordAccessPolicy for OperatorPolicy {
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(claims.sub, "user:viewer");
            entities
                .into_iter()
                .filter_map(|mut entity| {
                    if schema.name.as_str() == "Child" {
                        if self.redact_authorization_attribute {
                            entity.fields.remove("blocked");
                        }
                        if entity.field("blocked") == Some(&DynamicValue::Boolean(true)) {
                            return None;
                        }
                        if entity.field("redacted") == Some(&DynamicValue::Boolean(true)) {
                            entity.fields.remove("label");
                        }
                    }
                    Some(entity)
                })
                .collect()
        })
    }
    fn can_modify<'a>(
        &'a self,
        _: &'a SchemaDefinition,
        _: &'a Claims,
        _: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
    fn can_delete<'a>(
        &'a self,
        _: &'a SchemaDefinition,
        _: &'a Claims,
        _: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}

async fn fixture(
    read_role: &str,
    label_annotation: &str,
    parent_annotation: &str,
    custom_policy: &str,
    operator: bool,
) -> Router {
    let mut schemas = schema_forge_dsl::parse(&format!(r#"
        @access(read: ["clerk"], write: ["manager"])
        schema Parent {{ title: text selected: -> Child denied: -> Child missing: -> Child linked: -> Child[] children: -> Child[] }}
        @display("label")
        @access(read: ["{read_role}"], write: ["manager"])
        schema Child {{ label: text {label_annotation} parent: -> Parent {parent_annotation} owner: text @owner blocked: boolean required redacted: boolean required }}
    "#)).unwrap();
    schemas[0]
        .fields
        .iter_mut()
        .find(|f| f.name.as_str() == "children")
        .unwrap()
        .derived_from = Some(FieldName::new("parent").unwrap());
    let backend = Arc::new(
        SurrealBackend::connect_memory("related", "related")
            .await
            .unwrap(),
    );
    for schema in &schemas {
        let plan = schema_forge_core::migration::DiffEngine::create_new(schema);
        backend
            .apply_migration(&schema.name, &plan.steps)
            .await
            .unwrap();
        backend.store_schema_metadata(schema).await.unwrap();
    }
    let parent = Entity::with_id(
        fixture_id("parent_one"),
        schemas[0].name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Parent".into())),
            (
                "selected".into(),
                DynamicValue::Ref(fixture_id("child_good")),
            ),
            (
                "denied".into(),
                DynamicValue::Ref(fixture_id("child_denied")),
            ),
            (
                "missing".into(),
                DynamicValue::Ref(fixture_id("child_absent")),
            ),
            (
                "linked".into(),
                DynamicValue::RefArray(vec![
                    fixture_id("child_good"),
                    fixture_id("child_denied"),
                    fixture_id("child_redacted"),
                    fixture_id("child_absent"),
                ]),
            ),
        ]),
    );
    backend.create(&parent).await.unwrap();
    for (id, owner, blocked, redacted, label) in [
        ("child_good", "viewer", false, false, "Visible"),
        ("child_denied", "other", true, false, "Private"),
        ("child_redacted", "viewer", false, true, "Redacted"),
    ] {
        let child = Entity::with_id(
            fixture_id(id),
            schemas[1].name.clone(),
            BTreeMap::from([
                ("label".into(), DynamicValue::Text(label.into())),
                ("parent".into(), DynamicValue::Ref(parent.id.clone())),
                ("owner".into(), DynamicValue::Text(owner.into())),
                ("blocked".into(), DynamicValue::Boolean(blocked)),
                ("redacted".into(), DynamicValue::Boolean(redacted)),
            ]),
        );
        backend.create(&child).await.unwrap();
    }
    let policy_dir = tempfile::tempdir().unwrap();
    std::fs::write(policy_dir.path().join("related.cedar"), custom_policy).unwrap();
    let store = schema_forge_acton::authz::PolicyStoreSnapshot::from_schemas(
        &schemas,
        Some(policy_dir.path()),
        schema_forge_acton::authz::RoleRanks::empty(),
        schema_forge_acton::authz::PrincipalClaimMappings::default(),
    )
    .unwrap();
    let service = acton_service::service_builder::ServiceBuilder::new()
        .with_config(Config::<SchemaForgeConfig>::default())
        .with_actor::<ForgeActor>()
        .with_actor::<schema_forge_acton::HookDispatchActor>()
        .build();
    let (tx, rx) = oneshot::channel();
    service
        .state()
        .actor::<ForgeActor>()
        .unwrap()
        .send(InitForge {
            registry: schemas
                .into_iter()
                .map(|s| (s.name.as_str().to_string(), s))
                .collect(),
            backend,
            tenant_config: None,
            record_access_policy: if operator {
                Some(Arc::new(OperatorPolicy {
                    redact_authorization_attribute: !custom_policy.is_empty(),
                }))
            } else {
                None
            },
            hook_dispatcher: None,
            storage_registry: Default::default(),
            policy_store: Some(Arc::new(schema_forge_acton::authz::PolicyStore::new(store))),
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    forge_routes().with_state(service.state().clone())
}

async fn read(app: &Router, method: Method, path: &str) -> Value {
    let claims = Claims {
        sub: "user:viewer".into(),
        roles: vec!["clerk".into()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    };
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    request.extensions_mut().insert(claims);
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn parent_views(app: &Router) -> Vec<Value> {
    let mut views = vec![];
    for (method, path) in [
        (
            Method::GET,
            "/schemas/Parent/entities/parent_one_00000000000000000000000000",
        ),
        (Method::GET, "/schemas/Parent/entities"),
        (Method::POST, "/schemas/Parent/entities/query"),
    ] {
        let body = read(app, method, path).await;
        views.push(if body.get("entities").is_some() {
            body["entities"][0]["fields"].clone()
        } else {
            body["fields"].clone()
        });
    }
    views
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_role_denial_hides_related_labels_and_inverse_ids_on_every_read() {
    let app = fixture("manager", "", "", "", false).await;
    for fields in parent_views(&app).await {
        assert!(fields.get("selected__display").is_none(), "{fields}");
        assert!(fields.get("linked__display").is_none(), "{fields}");
        assert!(fields.get("missing__display").is_none(), "{fields}");
        assert_eq!(fields["children"], json!([]));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn related_rows_use_complete_owner_and_custom_policy_attributes() {
    for policy in [
        r#"forbid(principal, action == Action::"ReadChild", resource is Child) when { !context.resource_is_placeholder && resource has owner && resource.owner != principal.id };"#,
        r#"forbid(principal, action == Action::"ReadChild", resource is Child) when { !context.resource_is_placeholder && resource.blocked };"#,
    ] {
        let app = fixture("clerk", "", "", policy, false).await;
        for fields in parent_views(&app).await {
            assert_eq!(fields["selected__display"], "Visible", "{fields}");
            assert!(fields.get("denied__display").is_none(), "{fields}");
            assert!(fields.get("missing__display").is_none(), "{fields}");
            let children = fields["children"].as_array().unwrap();
            assert_eq!(children.len(), 2, "{fields}");
            assert!(children.contains(&json!(fixture_id("child_good"))));
            assert!(!children.contains(&json!(fixture_id("child_denied"))));
            assert!(
                !fields["linked__display"].to_string().contains("Private"),
                "{fields}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_policy_filters_related_rows_and_display_fields() {
    let app = fixture("clerk", "", "", "", true).await;
    for fields in parent_views(&app).await {
        assert_eq!(fields["selected__display"], "Visible", "{fields}");
        assert!(fields.get("denied__display").is_none(), "{fields}");
        let labels = fields["linked__display"].to_string();
        assert!(
            !labels.contains("Private") && !labels.contains("Redacted"),
            "{fields}"
        );
        let children = fields["children"].as_array().unwrap();
        assert_eq!(children.len(), 2, "{fields}");
        assert!(!children.contains(&json!(fixture_id("child_denied"))));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn target_field_restrictions_apply_before_labels_and_inverse_ids() {
    for label_annotation in [r#"@field_access(read: ["manager"])"#, "@hidden"] {
        let app = fixture(
            "clerk",
            label_annotation,
            r#"@field_access(read: ["manager"])"#,
            "",
            false,
        )
        .await;
        for fields in parent_views(&app).await {
            assert!(fields.get("selected__display").is_none(), "{fields}");
            assert!(fields.get("linked__display").is_none(), "{fields}");
            assert_eq!(fields["children"], json!([]));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_redaction_cannot_erase_attributes_before_cedar_read_checks() {
    let policy = r#"forbid(principal, action == Action::"ReadChild", resource is Child) when { !context.resource_is_placeholder && resource.blocked };"#;
    let app = fixture("clerk", "", "", policy, true).await;
    for fields in parent_views(&app).await {
        assert_eq!(fields["selected__display"], "Visible", "{fields}");
        assert!(fields.get("denied__display").is_none(), "{fields}");
        let children = fields["children"].as_array().unwrap();
        assert_eq!(children.len(), 2, "{fields}");
        assert!(!children.contains(&json!(fixture_id("child_denied"))));
    }
}
