//! HTTP authorization and unsupported conditional-mutation contracts on SurrealDB.
use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{http::StatusCode, Router};
use schema_forge_acton::{
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    state::DynEntityStore,
    storage::StorageRegistry,
    ForgeActor,
};
use schema_forge_backend::{conditional::EntityRevision, entity::Entity, tenant::TenantConfig};
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldDefinition, FieldName, FieldType, SchemaDefinition,
    SchemaId, SchemaName, TenantKind, TextConstraints,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;

#[path = "support/conditional_entities.rs"]
pub mod conditional_entities;
use conditional_entities::*;

async fn fixture(owner: &str, roles: &[&str]) -> (Router, String) {
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("conditional", "conditional")
            .await
            .unwrap(),
    );
    fixture_with_backend(backend, owner, roles, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_deployment_settings_preserves_authorized_json_changes() {
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("patch", "deployment")
            .await
            .unwrap(),
    );
    deployment_settings_patch_contract(backend, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_field_authorization_uses_complete_resource_and_preserves_denied_fields() {
    let (app, path) = fixture("editor", &["editor", "manager"]).await;
    for fields in [
        serde_json::json!({"restricted": "First protected edit"}),
        serde_json::json!({"title": "Original", "restricted": "Second protected edit"}),
    ] {
        let (status, _, body) = request(&app, &path, "PATCH", None, fields.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["fields"]["restricted"], fields["restricted"]);
        let (status, _, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["fields"]["restricted"], fields["restricted"]);
        assert_eq!(body["fields"]["title"], "Original");
    }
    let (app, path) = fixture("editor", &["editor"]).await;
    let (status, _, body) = request(
        &app,
        &path,
        "PATCH",
        None,
        serde_json::json!({"title": "Permitted edit", "restricted": "Forbidden edit"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Permitted edit");
    assert_eq!(body["fields"]["restricted"], "Restricted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_conditional_mutations_do_not_change_or_delete_the_record() {
    let (app, path) = fixture("editor", &["editor"]).await;
    let token = EntityRevision::fresh();
    for method in ["PUT", "PATCH", "DELETE"] {
        let (status, headers, body) = request(
            &app,
            &path,
            method,
            Some(token.as_str()),
            serde_json::json!({"title":"Changed"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{method}: {body}");
        assert!(body
            .to_string()
            .contains("conditional_mutation_unsupported"));
        assert!(!headers.contains_key("entity-revision"));
    }
    let (status, headers, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key("entity-revision"));
    assert_eq!(body["fields"]["title"], "Original");
    let (status, _, body) = request(
        &app,
        &path,
        "PATCH",
        None,
        serde_json::json!({"title":"Ordinary edit"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Ordinary edit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_record_and_field_denial_precedes_token_or_capability_details() {
    for (owner, fields) in [
        ("other", serde_json::json!({"title":"Changed"})),
        ("editor", serde_json::json!({"restricted":"Changed"})),
    ] {
        let (app, path) = fixture(owner, &["editor"]).await;
        for condition in ["malformed".to_string(), EntityRevision::fresh().to_string()] {
            for method in ["PUT", "PATCH"] {
                let (status, headers, body) =
                    request(&app, &path, method, Some(&condition), fields.clone()).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
                assert!(!headers.contains_key("entity-revision"));
                assert!(!body.to_string().contains("revision_conflict"));
                assert!(!body
                    .to_string()
                    .contains("conditional_mutation_unsupported"));
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_schema_denial_precedes_token_validation() {
    let (app, path) = fixture("editor", &["unrelated"]).await;
    for method in ["PUT", "PATCH", "DELETE"] {
        let (status, headers, body) = request(
            &app,
            &path,
            method,
            Some("malformed"),
            serde_json::json!({"title":"Changed"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
        assert!(!headers.contains_key("entity-revision"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_delete_preserves_owner_denial_without_hiding_the_record() {
    let (app, path) = fixture("other", &["editor"]).await;
    // The owner denial is reached before the malformed conditional header is parsed, so the
    // refusal names neither a revision nor the conditional-support error.
    let (status, headers, body) = request(
        &app,
        &path,
        "DELETE",
        Some("malformed"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(!headers.contains_key("entity-revision"));
    assert!(!body
        .to_string()
        .contains("conditional_mutation_unsupported"));
    // `@owner` governs the write, never the read: the editor role the schema grants read to
    // still sees a record it does not own.
    let (status, headers, body) =
        request(&app, &path, "GET", Some("malformed"), serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Original");
    assert!(!headers.contains_key("entity-revision"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_put_computation_preserves_immutable_owner() {
    for (owner, roles) in [
        ("editor", vec!["editor"]),
        ("other", vec!["platform_admin"]),
    ] {
        let (app, path) = fixture(owner, &roles).await;
        for supplied_owner in [None, Some("attempted-transfer")] {
            let mut body = serde_json::json!({"title": "Replacement"});
            if let Some(value) = supplied_owner {
                body["owner"] = serde_json::json!(value);
            }
            let (status, _, response) = request(&app, &path, "PUT", None, body).await;
            assert_eq!(status, StatusCode::OK, "{response}");
            assert_eq!(response["fields"]["owner"], owner);
            assert!(response["fields"].get("name_key").is_none());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_mutations_honor_selected_tenant_before_condition_details() {
    use schema_forge_acton::middleware::tenant_scope::{middleware, TenantScopeState};

    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("TenantNote").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("title").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![
            Annotation::Access {
                read: vec!["editor".into()],
                write: vec!["editor".into()],
                delete: vec!["editor".into()],
                cross_tenant_read: vec![],
            },
            Annotation::Tenant(TenantKind::Child {
                parent: SchemaName::new("Organization").unwrap(),
            }),
        ],
    )
    .unwrap();
    let organization = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Organization").unwrap(),
        vec![FieldDefinition::new(
            FieldName::new("name").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )],
        vec![Annotation::Tenant(TenantKind::Root)],
    )
    .unwrap();
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("conditional_tenant", "conditional_tenant")
            .await
            .unwrap(),
    );
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    schema_forge_acton::DynSchemaBackend::apply_migration(
        backend.as_ref(),
        &schema.name,
        &plan.steps,
    )
    .await
    .unwrap();
    schema_forge_acton::DynSchemaBackend::store_schema_metadata(backend.as_ref(), &schema)
        .await
        .unwrap();
    let entity = Entity::with_id(
        EntityId::new("tenantnote"),
        schema.name.clone(),
        BTreeMap::from([
            (
                "title".into(),
                DynamicValue::Text("Program alpha record".into()),
            ),
            (
                "_tenant".into(),
                DynamicValue::Text("organization_alpha".into()),
            ),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &entity)
        .await
        .unwrap();
    let original = DynEntityStore::get(backend.as_ref(), &entity.schema, &entity.id)
        .await
        .unwrap();
    let tenant_config =
        TenantConfig::from_schemas(&[organization.clone(), schema.clone()]).unwrap();
    let scope = TenantScopeState {
        entity_store: backend.clone(),
        tenant_config: Arc::new(Some(tenant_config.clone())),
    };
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
            registry: HashMap::from([
                ("TenantNote".into(), schema),
                ("Organization".into(), organization),
            ]),
            backend: backend.clone(),
            tenant_config: Some(tenant_config),
            record_access_policy: None,
            hook_dispatcher: None,
            storage_registry: StorageRegistry::default(),
            policy_store: None,
            custom_policies_dir: None,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    let caller = Claims {
        sub: "user:editor".into(),
        roles: vec!["editor".into()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::from([(
            "tenant_chain".into(),
            serde_json::json!([
                {"schema":"Organization","entity_id":"organization_alpha"},
                {"schema":"Organization","entity_id":"organization_beta"},
            ]),
        )]),
    };
    let app = forge_routes()
        .layer(axum::middleware::from_fn_with_state(scope, middleware))
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let caller = caller.clone();
                async move {
                    req.extensions_mut().insert(caller);
                    next.run(req).await
                }
            },
        ))
        .with_state(service.state().clone());
    let path = format!("/schemas/TenantNote/entities/{}", entity.id);
    let alpha = Some("Organization:organization_alpha");
    let beta = Some("Organization:organization_beta");
    let (status, _, body) =
        request_in_tenant(&app, &path, "GET", None, serde_json::json!({}), alpha).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Program alpha record");

    let nonmatching = EntityRevision::fresh();
    for condition in ["malformed", nonmatching.as_str()] {
        for method in ["PUT", "PATCH", "DELETE"] {
            let (status, headers, body) = request_in_tenant(
                &app,
                &path,
                method,
                Some(condition),
                serde_json::json!({"title":"Unauthorized edit"}),
                beta,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
            assert!(!headers.contains_key("entity-revision"));
            let serialized = body.to_string();
            assert!(!serialized.contains("revision_conflict"));
            assert!(!serialized.contains("conditional_mutation_unsupported"));
            assert!(!serialized.contains("Invalid entity revision"));
            assert!(!serialized.contains("Program alpha record"));
        }
    }
    // Selecting the row's actual program reaches the capability gate instead.
    let (status, headers, body) = request_in_tenant(
        &app,
        &path,
        "PATCH",
        Some(nonmatching.as_str()),
        serde_json::json!({"title":"Still unchanged"}),
        alpha,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "conditional_mutation_unsupported");
    assert!(!headers.contains_key("entity-revision"));
    let final_row = DynEntityStore::get(backend.as_ref(), &entity.schema, &entity.id)
        .await
        .unwrap();
    assert_eq!(
        final_row, original,
        "denied mutations preserve the entire row"
    );
}

async fn write_pipeline_fixture() -> Router {
    write_pipeline_fixture_with_policy(&["editor"], None).await
}

async fn write_pipeline_fixture_with_policy(
    roles: &[&str],
    custom_policies_dir: Option<std::path::PathBuf>,
) -> Router {
    use schema_forge_backend::SchemaBackend;
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("writes", "writes")
            .await
            .unwrap(),
    );
    let schema = schema_forge_dsl::parse(r#"
        @access(read: ["editor", "manager"], write: ["editor", "manager"], delete: ["manager"])
        schema Line {
            title: text required
            stage: text required @default("'pending'")
            literal: text required default("literal")
            owner: text required @owner
            guarded: text required @default("'locked'") @field_access(read: ["editor", "manager"], write: ["manager"])
            optional: text @require("optional == null || size(optional) > 2", "optional too short")
            number: text @field_access(read: ["editor", "manager"], write: ["manager"])
            status: text @default("'pending'") @require("status != 'live' || number != null", "live needs number")
            has_number: boolean required @compute("number != null") @field_access(read: ["editor", "manager"], write: ["manager"])
        }
    "#).unwrap().remove(0);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    app_with_policies(backend, schema, roles, custom_policies_dir).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_rules_observe_only_authorized_input_and_keep_server_values() {
    let app = write_pipeline_fixture().await;
    let base = "/schemas/Line/entities";
    let (status, _, body) = request(
        &app,
        base,
        "POST",
        None,
        serde_json::json!({"title":"line", "number":"forbidden", "status":"live"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _, body) = request(
        &app,
        base,
        "POST",
        None,
        serde_json::json!({"title":"line", "number":"forbidden"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["fields"]["stage"], "pending");
    assert_eq!(body["fields"]["literal"], "literal");
    assert_eq!(body["fields"]["owner"], "editor");
    assert_eq!(body["fields"]["has_number"], false);
    assert!(body["fields"]["number"].is_null());
    let path = format!("{base}/{}", body["id"].as_str().unwrap());
    let (status, _, body) = request(
        &app,
        &path,
        "PATCH",
        None,
        serde_json::json!({"number":"forbidden", "status":"live"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _, body) = request(
        &app,
        &path,
        "PATCH",
        None,
        serde_json::json!({"number":"forbidden"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["fields"]["number"].is_null());
    assert_eq!(body["fields"]["has_number"], false);
    let (status, _, body) = request(&app, &path, "PUT", None,
        serde_json::json!({"title":"updated", "stage":"pending", "literal":"literal", "status":"live", "number":"forbidden"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_fields_reject_null_and_put_does_not_apply_create_defaults() {
    let app = write_pipeline_fixture().await;
    let base = "/schemas/Line/entities";
    let (status, _, body) =
        request(&app, base, "POST", None, serde_json::json!({"title":null})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(!body.to_string().contains("PUT"));
    let (status, _, body) = request(
        &app,
        base,
        "POST",
        None,
        serde_json::json!({"title":"line"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = format!("{base}/{}", body["id"].as_str().unwrap());
    let (status, _, body) = request(
        &app,
        &path,
        "PATCH",
        None,
        serde_json::json!({"guarded":null}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "denied null must not be validated as accepted input: {body}"
    );
    assert_eq!(body["fields"]["guarded"], "locked");
    for method in ["PATCH", "PUT"] {
        let (status, _, body) = request(
            &app,
            &path,
            method,
            None,
            serde_json::json!({"title":null, "stage":"pending", "literal":"literal"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{method}: {body}");
    }
    let (status, _, body) = request(
        &app,
        &path,
        "PUT",
        None,
        serde_json::json!({"title":"updated"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("stage"));
    let (status, _, body) = request(
        &app,
        &path,
        "PUT",
        None,
        serde_json::json!({"title":"updated", "stage":"pending", "literal":"literal"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["owner"], "editor");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_field_authorization_accepts_defaults_but_fails_closed_on_missing_policy_attributes()
{
    let base = "/schemas/Line/entities";
    let app = write_pipeline_fixture_with_policy(&["manager"], None).await;
    let (status, _, body) = request(
        &app,
        base,
        "POST",
        None,
        serde_json::json!({"title":"line", "number":"allowed", "status":"live"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["fields"]["number"], "allowed");
    assert_eq!(body["fields"]["has_number"], true);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("custom.cedar"),
        r#"
        forbid(principal, action == Action::"WriteFieldLine_number", resource is Line)
        when { resource.stage == "blocked" };
    "#,
    )
    .unwrap();
    let app =
        write_pipeline_fixture_with_policy(&["manager"], Some(dir.path().to_path_buf())).await;
    for stage in [None, Some("blocked")] {
        let mut fields = serde_json::json!({"title":"line", "number":"allowed"});
        if let Some(stage) = stage {
            fields["stage"] = stage.into();
        }
        let (status, _, body) = request(&app, base, "POST", None, fields).await;
        if stage.is_none() {
            // An errored forbid must not be bypassed by the generated role permit.
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        } else {
            assert_eq!(status, StatusCode::CREATED, "{body}");
            assert!(body["fields"]["number"].is_null());
            assert_eq!(body["fields"]["has_number"], false);
        }
    }
    let (status, _, body) = request(
        &app,
        base,
        "POST",
        None,
        serde_json::json!({"title":"line", "stage":"open", "number":"allowed"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["fields"]["has_number"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_omission_matches_persisted_values_and_preserves_denied_fields() {
    use schema_forge_backend::SchemaBackend;
    for role in ["editor", "manager"] {
        let backend = Arc::new(
            schema_forge_surrealdb::test_support::connect("put", "put")
                .await
                .unwrap(),
        );
        let schema = schema_forge_dsl::parse(
            r#"
            @access(read: ["editor", "manager"], write: ["editor", "manager"], delete: ["manager"])
            schema Contact {
                title: text required
                number: text @field_access(read: ["editor", "manager"], write: ["manager"])
                has_number: boolean @compute("number != null")
            }
        "#,
        )
        .unwrap()
        .remove(0);
        let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
        backend
            .apply_migration(&schema.name, &plan.steps)
            .await
            .unwrap();
        backend.store_schema_metadata(&schema).await.unwrap();
        let seed = Entity::new(
            schema.name.clone(),
            BTreeMap::from([
                ("title".into(), DynamicValue::Text("original".into())),
                ("number".into(), DynamicValue::Text("stored".into())),
                ("has_number".into(), DynamicValue::Boolean(true)),
            ]),
        );
        DynEntityStore::create(backend.as_ref(), &seed)
            .await
            .unwrap();
        let app = app_with_backend(backend, schema, &[role]).await;
        let path = format!("/schemas/Contact/entities/{}", seed.id);
        let (status, _, body) = request(
            &app,
            &path,
            "PUT",
            None,
            serde_json::json!({"title":"updated"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{role}: {body}");
        assert_eq!(body["fields"]["has_number"], role == "editor", "{body}");
        if role == "editor" {
            assert_eq!(body["fields"]["number"], "stored");
        } else {
            assert!(body["fields"]["number"].is_null());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_inputs_cannot_authorize_other_fields() {
    use schema_forge_backend::SchemaBackend;
    let backend = Arc::new(
        schema_forge_surrealdb::test_support::connect("field_auth", "field_auth")
            .await
            .unwrap(),
    );
    let schema = schema_forge_dsl::parse(
        r#"
        @access(read: ["editor"], write: ["editor"], delete: ["editor"])
        schema Pair {
            title: text required
            value: text @field_access(read: ["editor"], write: ["editor"])
            gate: text @field_access(read: ["editor"], write: ["manager"])
        }
    "#,
    )
    .unwrap()
    .remove(0);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    let seed = Entity::new(
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("original".into())),
            ("value".into(), DynamicValue::Text("original".into())),
            ("gate".into(), DynamicValue::Text("locked".into())),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &seed)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("custom.cedar"),
        r#"
        forbid(principal, action == Action::"WriteFieldPair_value", resource is Pair)
        when { !(resource has gate && resource.gate == "unlocked") };
    "#,
    )
    .unwrap();
    let app = app_with_policies(backend, schema, &["editor"], Some(dir.path().to_path_buf())).await;
    let path = format!("/schemas/Pair/entities/{}", seed.id);
    for method in ["PATCH", "PUT"] {
        let (status, _, body) = request(
            &app,
            &path,
            method,
            None,
            serde_json::json!({"title":"updated", "value":"attacker", "gate":"unlocked"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["fields"]["value"], "original");
        assert_eq!(body["fields"]["gate"], "locked");
    }
    let (status, _, body) = request(
        &app,
        "/schemas/Pair/entities",
        "POST",
        None,
        serde_json::json!({"title":"new", "value":"attacker", "gate":"unlocked"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["fields"]["value"].is_null());
    assert!(body["fields"]["gate"].is_null());
}
