//! Policies can distinguish preflight placeholders without treating real defaults as synthetic.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use acton_service::middleware::Claims;
use schema_forge_acton::authz::{
    authorize, authorize_field, namespace::ActionVerb, FieldDirection, PolicyStore,
    PolicyStoreSnapshot, PrincipalClaimMappings, RoleRanks,
};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition};

fn claims() -> Claims {
    Claims {
        sub: "user:reviewer".into(),
        roles: vec!["reviewer".into()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::from([("resource_is_placeholder".into(), serde_json::json!(true))]),
    }
}

fn fixture(required: bool, policy: &str) -> (SchemaDefinition, Arc<PolicyStore>) {
    let modifier = if required { "required" } else { "" };
    let schema = schema_forge_dsl::parse(&format!(
        r#"
        @access(read: ["reviewer"], write: ["reviewer"], delete: ["reviewer"])
        schema Notice {{
            visible: boolean {modifier}
            title: text required @field_access(read: ["reviewer"], write: ["reviewer"])
            amount: integer
        }}
    "#
    ))
    .unwrap()
    .remove(0);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("custom.cedar"), policy).unwrap();
    let snapshot = PolicyStoreSnapshot::from_schemas(
        std::slice::from_ref(&schema),
        Some(dir.path()),
        RoleRanks::empty(),
        PrincipalClaimMappings::default(),
    )
    .unwrap();
    (schema, Arc::new(PolicyStore::new(snapshot)))
}

fn record(schema: &SchemaDefinition, visible: Option<bool>) -> Entity {
    let mut fields = BTreeMap::from([("title".into(), DynamicValue::Text(String::new()))]);
    if let Some(visible) = visible {
        fields.insert("visible".into(), DynamicValue::Boolean(visible));
    }
    Entity::with_id(EntityId::new("notice"), schema.name.clone(), fields)
}

const GUARDED_FORBID: &str = r#"
    forbid(principal, action == Action::"ReadNotice", resource is Notice)
    when {
        !context.resource_is_placeholder &&
        !(resource has visible && resource.visible)
    };
"#;

#[test]
fn guarded_read_forbid_distinguishes_required_optional_and_real_default_values() {
    for required in [false, true] {
        let (schema, store) = fixture(required, GUARDED_FORBID);
        let caller = claims();
        assert!(
            authorize(&store, Some(&caller), ActionVerb::Read, &schema, None)
                .unwrap()
                .is_allow()
        );
        for visible in [false, true] {
            let entity = record(&schema, Some(visible));
            let decision = authorize(
                &store,
                Some(&caller),
                ActionVerb::Read,
                &schema,
                Some(&entity),
            )
            .unwrap();
            assert_eq!(decision.is_allow(), visible);
            assert!(decision.errors.is_empty());
        }
        if !required {
            assert!(!authorize(
                &store,
                Some(&caller),
                ActionVerb::Read,
                &schema,
                Some(&record(&schema, None))
            )
            .unwrap()
            .is_allow());
        }
    }
}

#[test]
fn unguarded_forbid_keeps_required_placeholder_denial() {
    for required in [false, true] {
        let (schema, store) = fixture(
            required,
            r#"
            forbid(principal, action == Action::"ReadNotice", resource is Notice)
            when { resource has visible && !resource.visible };
        "#,
        );
        let decision = authorize(&store, Some(&claims()), ActionVerb::Read, &schema, None).unwrap();
        assert_eq!(decision.is_allow(), !required);
        assert!(decision.errors.is_empty());
    }
}

#[test]
fn custom_read_permit_must_explicitly_admit_preflight_and_concrete_records() {
    let (schema, store) = fixture(
        true,
        r#"
        permit(principal in Forge::Group::"guest_reviewer", action == Action::"ReadNotice", resource is Notice)
        when { context.resource_is_placeholder || (resource has visible && resource.visible) };
    "#,
    );
    let mut caller = claims();
    caller.roles = vec!["guest_reviewer".into()];
    assert!(
        authorize(&store, Some(&caller), ActionVerb::Read, &schema, None)
            .unwrap()
            .is_allow()
    );
    for visible in [false, true] {
        assert_eq!(
            authorize(
                &store,
                Some(&caller),
                ActionVerb::Read,
                &schema,
                Some(&record(&schema, Some(visible)))
            )
            .unwrap()
            .is_allow(),
            visible
        );
    }
}

#[test]
fn every_application_action_requires_explicit_context_for_manual_cedar_requests() {
    use cedar_policy::{Context, Request, RestrictedExpression};
    let (_, store) = fixture(true, "");
    let snapshot = store.current();
    for action in [
        "ReadNotice",
        "ListNotice",
        "CreateNotice",
        "UpdateNotice",
        "DeleteNotice",
        "ExportNotice",
        "ReadFieldNotice_title",
        "WriteFieldNotice_title",
    ] {
        let principal: cedar_policy::EntityUid = r#"Forge::Principal::"reviewer""#.parse().unwrap();
        let action: cedar_policy::EntityUid = format!("Action::\"{action}\"").parse().unwrap();
        let resource: cedar_policy::EntityUid = r#"Notice::"_any""#.parse().unwrap();
        assert!(Request::new(
            principal.clone(),
            action.clone(),
            resource.clone(),
            Context::empty(),
            Some(&snapshot.schema)
        )
        .is_err());
        let context = Context::from_pairs([(
            "resource_is_placeholder".into(),
            RestrictedExpression::new_bool(true),
        )])
        .unwrap();
        assert!(Request::new(principal, action, resource, context, Some(&snapshot.schema)).is_ok());
    }
}

#[test]
fn field_checks_always_describe_a_concrete_resource() {
    let (schema, store) = fixture(
        true,
        r#"
        forbid(principal, action in [Action::"ReadFieldNotice_title", Action::"WriteFieldNotice_title"], resource is Notice)
        when { !context.resource_is_placeholder };
    "#,
    );
    let entity = record(&schema, Some(false));
    for direction in [FieldDirection::Read, FieldDirection::Write] {
        let decision = authorize_field(
            &store,
            Some(&claims()),
            &schema,
            &entity,
            "title",
            direction,
        )
        .unwrap();
        assert!(!decision.is_allow());
        assert!(decision.errors.is_empty());
        assert!(!decision.matched_policies.is_empty());
    }
}

#[derive(Clone, Default)]
struct CapturedEvents(Arc<Mutex<Vec<BTreeMap<String, String>>>>);

#[derive(Default)]
struct Fields(BTreeMap<String, String>);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}
impl tracing::Subscriber for CapturedEvents {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        fields
            .0
            .insert("level".into(), event.metadata().level().to_string());
        self.0.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn denial_logs_identify_placeholder_concrete_resource_and_matching_forbid() {
    let (schema, store) = fixture(
        true,
        r#"
        forbid(principal, action == Action::"ReadNotice", resource is Notice)
        when { resource has visible && !resource.visible };
    "#,
    );
    let caller = claims();
    let entity = record(&schema, Some(false));
    let capture = CapturedEvents::default();
    let decisions = tracing::subscriber::with_default(capture.clone(), || {
        [None, Some(&entity)].map(|resource| {
            authorize(&store, Some(&caller), ActionVerb::Read, &schema, resource).unwrap()
        })
    });
    let events = capture.0.lock().unwrap();
    let denials: Vec<_> = events
        .iter()
        .filter(|event| {
            event
                .get("message")
                .is_some_and(|message| message == "authz deny")
        })
        .collect();
    assert_eq!(denials.len(), 2, "{events:?}");
    for (index, event) in denials.iter().enumerate() {
        assert_eq!(event["level"], "WARN");
        assert_eq!(event["resource_is_placeholder"], (index == 0).to_string());
        assert_eq!(
            event["matched_policies"],
            format!("{:?}", decisions[index].matched_policies)
        );
        assert!(!decisions[index].matched_policies.is_empty());
    }
    assert_eq!(denials[0]["resource_uid"], "Notice::\"_any\"");
    assert_eq!(
        denials[1]["resource_uid"],
        format!("Notice::\"{}\"", entity.id)
    );
}

#[test]
fn evaluation_error_rejection_is_logged_as_denial_even_with_matching_permit() {
    let (schema, store) = fixture(
        true,
        r#"
        forbid(principal, action == Action::"ReadNotice", resource is Notice)
        when { resource has amount && resource.amount + 1 < 0 };
    "#,
    );
    let mut entity = record(&schema, Some(true));
    entity
        .fields
        .insert("amount".into(), DynamicValue::Integer(i64::MAX));
    let capture = CapturedEvents::default();
    let decision = tracing::subscriber::with_default(capture.clone(), || {
        authorize(
            &store,
            Some(&claims()),
            ActionVerb::Read,
            &schema,
            Some(&entity),
        )
        .unwrap()
    });
    assert!(decision.allowed);
    assert!(!decision.is_allow());
    assert!(!decision.errors.is_empty());
    let events = capture.0.lock().unwrap();
    assert!(events.iter().any(|event| event
        .get("message")
        .is_some_and(|message| message == "authz deny")
        && event["level"] == "WARN"));
    assert!(!events.iter().any(|event| event
        .get("message")
        .is_some_and(|message| message == "authz allow")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guarded_read_policy_preserves_http_preflight_and_record_denials() {
    use acton_service::{
        config::Config, prelude::ActorHandleInterface, service_builder::ServiceBuilder,
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Extension,
    };
    use http_body_util::BodyExt;
    use schema_forge_acton::{
        config::SchemaForgeConfig,
        messages::{InitForge, ReplyChannel},
        routes::forge_routes,
        state::DynForgeBackend,
        ForgeActor,
    };
    use schema_forge_core::migration::DiffEngine;
    use tokio::sync::oneshot;
    use tower::ServiceExt;

    for required in [false, true] {
        let (schema, store) = fixture(required, GUARDED_FORBID);
        let backend: Arc<dyn DynForgeBackend> = Arc::new(
            schema_forge_surrealdb::SurrealBackend::connect_memory("test", "context_http")
                .await
                .unwrap(),
        );
        let plan = DiffEngine::create_new(&schema);
        backend
            .apply_migration(&schema.name, &plan.steps)
            .await
            .unwrap();
        backend.store_schema_metadata(&schema).await.unwrap();
        let visible = record(&schema, Some(true));
        let hidden = record(&schema, Some(false));
        backend.create(&visible).await.unwrap();
        backend.create(&hidden).await.unwrap();
        let service = ServiceBuilder::new()
            .with_config(Config::<SchemaForgeConfig>::default())
            .with_actor::<ForgeActor>()
            .build();
        let forge = service.state().actor::<ForgeActor>().unwrap();
        let (tx, rx) = oneshot::channel();
        forge
            .send(InitForge {
                registry: HashMap::from([(schema.name.to_string(), schema)]),
                backend,
                tenant_config: None,
                record_access_policy: None,
                hook_dispatcher: None,
                storage_registry: schema_forge_acton::storage::StorageRegistry::default(),
                policy_store: Some(store),
                custom_policies_dir: None,
                reply: ReplyChannel::new(tx),
            })
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .unwrap()
            .unwrap();
        let app = forge_routes()
            .layer(Extension(claims()))
            .with_state(service.state().clone());
        for (path, expected) in [
            ("/schemas/Notice/entities".into(), StatusCode::OK),
            (
                format!("/schemas/Notice/entities/{}", visible.id),
                StatusCode::OK,
            ),
            (
                format!("/schemas/Notice/entities/{}", hidden.id),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path}, required={required}");
            let body: serde_json::Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            if path == "/schemas/Notice/entities" {
                assert_eq!(body["count"], 1);
                assert_eq!(body["total_count"], 1);
                assert_eq!(body["entities"][0]["id"], visible.id.as_str());
            }
        }
    }
}
