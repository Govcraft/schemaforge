//! HTTP record revision contracts for unsupported and explicitly disposable
//! PostgreSQL storage, including authorization before token/capability errors.
use acton_service::{
    config::Config, middleware::Claims, prelude::ActorHandleInterface,
    service_builder::ServiceBuilder,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use schema_forge_acton::{
    config::SchemaForgeConfig,
    messages::{InitForge, ReplyChannel},
    routes::forge_routes,
    state::{DynEntityStore, DynForgeBackend},
    storage::StorageRegistry,
    ForgeActor,
};
use schema_forge_backend::{conditional::EntityRevision, entity::Entity, tenant::TenantConfig};
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldAnnotation, FieldDefinition, FieldModifier, FieldName,
    FieldType, SchemaDefinition, SchemaId, SchemaName, TenantKind, TextConstraints,
};
use schema_forge_surrealdb::SurrealBackend;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;
use tower::ServiceExt;

async fn fixture(owner: &str, roles: &[&str]) -> (Router, String) {
    let backend = Arc::new(
        SurrealBackend::connect_memory("conditional", "conditional")
            .await
            .unwrap(),
    );
    fixture_with_backend(backend, owner, roles, false).await
}

async fn fixture_with_backend(
    backend: Arc<dyn DynForgeBackend>,
    owner: &str,
    roles: &[&str],
    prepare_revisions: bool,
) -> (Router, String) {
    let schema = SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Note").unwrap(),
        vec![
            FieldDefinition::with_modifiers(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required, FieldModifier::Unique],
            ),
            FieldDefinition::with_annotations(
                FieldName::new("owner").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::Owner],
            ),
            FieldDefinition::with_annotations(
                FieldName::new("name_key").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![
                    FieldAnnotation::Hidden,
                    FieldAnnotation::Compute {
                        expr: "string(size(owner)) + ':' + owner + title".into(),
                    },
                ],
            ),
            FieldDefinition::with_annotations(
                FieldName::new("restricted").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::FieldAccess {
                    read: vec!["editor".into()],
                    write: vec!["manager".into()],
                }],
            ),
        ],
        vec![Annotation::Access {
            read: vec!["editor".into()],
            write: vec!["editor".into()],
            delete: vec!["editor".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap();
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    if prepare_revisions {
        backend
            .prepare_record_revisions(&schema.name)
            .await
            .unwrap();
    }
    let entity = Entity::with_id(
        EntityId::new("note"),
        schema.name.clone(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Original".into())),
            ("owner".into(), DynamicValue::Text(owner.into())),
            ("restricted".into(), DynamicValue::Text("Restricted".into())),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &entity)
        .await
        .unwrap();
    let app = app_with_backend(backend, schema, roles).await;
    (app, format!("/schemas/Note/entities/{}", entity.id))
}

async fn app_with_backend(
    backend: Arc<dyn DynForgeBackend>,
    schema: SchemaDefinition,
    roles: &[&str],
) -> Router {
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
            registry: HashMap::from([(schema.name.to_string(), schema)]),
            backend,
            tenant_config: None,
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
        roles: roles.iter().map(|role| (*role).into()).collect(),
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
    forge_routes()
        .layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                let caller = caller.clone();
                async move {
                    req.extensions_mut().insert(caller);
                    next.run(req).await
                }
            },
        ))
        .with_state(service.state().clone())
}

async fn request(
    app: &Router,
    path: &str,
    method: &str,
    condition: Option<&str>,
    fields: serde_json::Value,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    request_in_tenant(app, path, method, condition, fields, None).await
}

async fn request_in_tenant(
    app: &Router,
    path: &str,
    method: &str,
    condition: Option<&str>,
    fields: serde_json::Value,
    tenant: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(condition) = condition {
        request = request.header("if-entity-revision", condition);
    }
    if let Some(tenant) = tenant {
        request = request.header("x-active-tenant", tenant);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::json!({"fields": fields}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn deployment_settings_patch_contract(backend: Arc<dyn DynForgeBackend>, conditional: bool) {
    let schema = schema_forge_dsl::parse(r#"
        @version(1)
        @display("key")
        @access(read: ["public"], write: ["platform_admin"], delete: ["platform_admin"])
        schema CharterDeploymentSettings {
            key: enum("deployment") required unique
            published_profile: json required
            draft_profile: json @field_access(read: ["platform_admin"], write: ["platform_admin"])
            previous_profile: json @field_access(read: ["platform_admin"], write: ["platform_admin"])
        }
    "#).unwrap().remove(0);
    let plan = schema_forge_core::migration::DiffEngine::create_new(&schema);
    backend
        .apply_migration(&schema.name, &plan.steps)
        .await
        .unwrap();
    backend.store_schema_metadata(&schema).await.unwrap();
    if conditional {
        backend
            .prepare_record_revisions(&schema.name)
            .await
            .unwrap();
    }
    let app = app_with_backend(backend, schema, &["platform_admin"]).await;
    let collection = "/schemas/CharterDeploymentSettings/entities";
    let (status, _, body) = request(
        &app,
        collection,
        "POST",
        None,
        serde_json::json!({"key": "deployment", "published_profile": {"version": 1},
            "draft_profile": {"version": 1}}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = format!("{collection}/{}", body["id"].as_str().unwrap());
    for version in [2, 3] {
        let (status, headers, body) =
            request(&app, &path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let revision = conditional.then(|| headers["entity-revision"].to_str().unwrap().to_owned());
        let mut fields = serde_json::json!({"draft_profile": {"version": version}});
        if version == 3 {
            fields["key"] = serde_json::json!("deployment");
        }
        let (status, _, body) =
            request(&app, &path, "PATCH", revision.as_deref(), fields.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["fields"]["draft_profile"], fields["draft_profile"]);
        let (status, headers, body) =
            request(&app, &path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["fields"]["draft_profile"], fields["draft_profile"]);
        assert_eq!(
            body["fields"]["published_profile"],
            serde_json::json!({"version": 1})
        );
        assert_eq!(body["fields"]["key"], "deployment");
        if let Some(revision) = revision {
            assert_ne!(headers["entity-revision"].to_str().unwrap(), revision);
            let (status, _, body) = request(
                &app,
                &path,
                "PATCH",
                Some(&revision),
                serde_json::json!({"draft_profile": {"version": 999}}),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(body["reason"], "revision_conflict");
            let (_, _, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
            assert_eq!(body["fields"]["draft_profile"], fields["draft_profile"]);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_deployment_settings_preserves_authorized_json_changes() {
    let backend = Arc::new(
        SurrealBackend::connect_memory("patch", "deployment")
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
    assert!(!body.to_string().contains("conditional_mutation_unsupported"));
    // `@owner` governs the write, never the read: the editor role the schema grants read to
    // still sees a record it does not own.
    let (status, headers, body) = request(
        &app,
        &path,
        "GET",
        Some("malformed"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Original");
    assert!(!headers.contains_key("entity-revision"));
}

#[cfg(feature = "postgres")]
fn response_revision(headers: &axum::http::HeaderMap) -> EntityRevision {
    assert!(!headers.contains_key("etag"));
    assert_eq!(headers["access-control-expose-headers"], "Entity-Revision");
    headers["entity-revision"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[cfg(feature = "postgres")]
fn assert_revision_conflict(result: &(StatusCode, axum::http::HeaderMap, serde_json::Value)) {
    assert_eq!(result.0, StatusCode::CONFLICT, "{}", result.2);
    assert!(!result.1.contains_key("entity-revision"));
    let serialized = result.2.to_string();
    assert_eq!(result.2["reason"], "revision_conflict");
    assert_eq!(result.2.as_object().unwrap().len(), 3);
    assert!(result.2.get("revision").is_none());
    assert!(!serialized.contains("fields"));
    assert!(!serialized.contains("restricted"));
}

/// The caller supplies a URL whose search_path names a fresh disposable
/// namespace and drops it afterward, including on failure. Never use an
/// existing application's unscoped URL for this test.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a scoped disposable PostgreSQL URL and SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1"]
async fn postgres_http_revisions_guard_updates_noops_races_and_deletes() {
    use schema_forge_acton::state::DynSchemaBackend;
    assert_eq!(
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE").as_deref(),
        Ok("1"),
        "explicit disposable database authorization is required",
    );
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL")
        .expect("SCHEMAFORGE_TEST_POSTGRES_URL must name a disposable namespace");
    // Connection failures can carry the URL. Keep credentials out of test logs.
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("could not connect to the disposable PostgreSQL namespace")),
    );
    deployment_settings_patch_contract(backend.clone(), true).await;
    let (app, path) = fixture_with_backend(backend.clone(), "editor", &["editor"], true).await;

    let (status, headers, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Original");
    let initial = response_revision(&headers);
    // A conditional write must preserve ordinary uniqueness semantics, and a
    // rejected write must roll back both fields and the revision marker.
    let occupied = Entity::with_id(
        EntityId::new("note"),
        SchemaName::new("Note").unwrap(),
        BTreeMap::from([
            ("title".into(), DynamicValue::Text("Taken".into())),
            ("owner".into(), DynamicValue::Text("editor".into())),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &occupied)
        .await
        .unwrap();
    for method in ["PATCH", "PUT"] {
        let (status, headers, body) = request(
            &app,
            &path,
            method,
            Some(initial.as_str()),
            serde_json::json!({"title": "Taken"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["reason"], "unique_violation");
        assert!(body.get("field").is_none());
        assert!(body.get("schema").is_none());
        assert!(!headers.contains_key("entity-revision"));
        let (status, headers, body) =
            request(&app, &path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["fields"]["title"], "Original");
        assert_eq!(response_revision(&headers), initial);
    }
    let (status, headers, body) = request(
        &app,
        &path,
        "PATCH",
        Some(initial.as_str()),
        serde_json::json!({"title":"First edit"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "First edit");
    assert_eq!(body["fields"]["restricted"], "Restricted");
    let patched = response_revision(&headers);
    assert_ne!(initial, patched);
    assert_revision_conflict(
        &request(
            &app,
            &path,
            "PATCH",
            Some(initial.as_str()),
            serde_json::json!({"title":"Stale edit"}),
        )
        .await,
    );

    let (status, headers, body) = request(
        &app,
        &path,
        "PATCH",
        Some(patched.as_str()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let no_op = response_revision(&headers);
    assert_ne!(
        patched, no_op,
        "an accepted conditional no-op advances the revision"
    );
    let (status, headers, body) = request(
        &app,
        &path,
        "PUT",
        Some(no_op.as_str()),
        serde_json::json!({"title":"Put edit"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Put edit");
    let put = response_revision(&headers);
    assert_ne!(no_op, put);

    let (left, right) = tokio::join!(
        request(
            &app,
            &path,
            "PATCH",
            Some(put.as_str()),
            serde_json::json!({"title":"Left winner"})
        ),
        request(
            &app,
            &path,
            "PATCH",
            Some(put.as_str()),
            serde_json::json!({"title":"Right winner"})
        ),
    );
    let (winner, loser) = if left.0 == StatusCode::OK {
        (&left, &right)
    } else {
        (&right, &left)
    };
    assert_eq!(winner.0, StatusCode::OK, "{}", winner.2);
    assert_revision_conflict(loser);
    let won = response_revision(&winner.1);
    let (status, headers, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["fields"]["title"], winner.2["fields"]["title"]);
    assert_eq!(response_revision(&headers), won);
    // A stale empty patch must not escape through the no-op early return.
    assert_revision_conflict(
        &request(
            &app,
            &path,
            "PATCH",
            Some(put.as_str()),
            serde_json::json!({}),
        )
        .await,
    );

    // Ordinary clients still write successfully, invalidating earlier revisions.
    let (status, _, body) = request(
        &app,
        &path,
        "PATCH",
        None,
        serde_json::json!({"title":"Ordinary write"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_revision_conflict(
        &request(
            &app,
            &path,
            "PUT",
            Some(won.as_str()),
            serde_json::json!({"title":"Lost update"}),
        )
        .await,
    );

    // Even supported storage must authorize before disclosing token freshness.
    let other = Entity::with_id(
        EntityId::new("note"),
        SchemaName::new("Note").unwrap(),
        BTreeMap::from([
            (
                "title".into(),
                DynamicValue::Text("Other owner record".into()),
            ),
            ("owner".into(), DynamicValue::Text("other".into())),
            (
                "restricted".into(),
                DynamicValue::Text("Private field".into()),
            ),
        ]),
    );
    DynEntityStore::create(backend.as_ref(), &other)
        .await
        .unwrap();
    let other_path = format!("/schemas/Note/entities/{}", other.id);
    // `@owner` governs the mutations, not the read. Every attempt to change a record the caller
    // does not own is refused before the conditional header is honored, and the refusal leaks
    // neither a revision nor the record it was denied.
    for method in ["PUT", "PATCH", "DELETE"] {
        let (status, headers, body) = request(
            &app,
            &other_path,
            method,
            Some(initial.as_str()),
            serde_json::json!({"title":"Denied"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
        assert!(!headers.contains_key("entity-revision"));
        assert!(!body.to_string().contains("revision_conflict"));
        assert!(!body.to_string().contains("Other owner record"));
    }
    // The read the schema grants to `editor` still succeeds, and says plainly that this caller
    // may not change what it is reading.
    let (status, _, body) = request(&app, &other_path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Other owner record");
    assert_eq!(body["permissions"]["update"], false, "{body}");
    assert_eq!(body["permissions"]["delete"], false, "{body}");
    let (status, headers, body) = request(
        &app,
        &path,
        "PATCH",
        Some(initial.as_str()),
        serde_json::json!({"restricted":"Denied"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(!headers.contains_key("entity-revision"));
    assert!(!body.to_string().contains("revision_conflict"));

    assert_revision_conflict(
        &request(
            &app,
            &path,
            "DELETE",
            Some(initial.as_str()),
            serde_json::json!({}),
        )
        .await,
    );
    let (status, headers, body) = request(&app, &path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fields"]["title"], "Ordinary write");
    let current = response_revision(&headers);
    let (status, headers, body) = request(
        &app,
        &path,
        "DELETE",
        Some(current.as_str()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!headers.contains_key("entity-revision"));
    let (status, headers, _) = request(&app, &path, "GET", None, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!headers.contains_key("entity-revision"));

    // Different records with independent valid revisions still compete for
    // one unique value. The loser is a business conflict, not an outage.
    let mut contenders = Vec::new();
    for title in ["Left original", "Right original"] {
        let entity = Entity::with_id(
            EntityId::new("note"),
            SchemaName::new("Note").unwrap(),
            BTreeMap::from([
                ("title".into(), DynamicValue::Text(title.into())),
                ("owner".into(), DynamicValue::Text("editor".into())),
            ]),
        );
        DynEntityStore::create(backend.as_ref(), &entity)
            .await
            .unwrap();
        let path = format!("/schemas/Note/entities/{}", entity.id);
        let (status, headers, _) = request(&app, &path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);
        contenders.push((path, response_revision(&headers), title));
    }
    let (left, right) = tokio::join!(
        request(
            &app,
            &contenders[0].0,
            "PATCH",
            Some(contenders[0].1.as_str()),
            serde_json::json!({"title":"Shared name"})
        ),
        request(
            &app,
            &contenders[1].0,
            "PATCH",
            Some(contenders[1].1.as_str()),
            serde_json::json!({"title":"Shared name"})
        ),
    );
    assert_eq!(
        [left.0, right.0]
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    for (result, (path, baseline, original)) in [left, right].iter().zip(&contenders) {
        let (status, headers, body) = request(&app, path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);
        if result.0 == StatusCode::OK {
            assert_eq!(body["fields"]["title"], "Shared name");
            assert_ne!(&response_revision(&headers), baseline);
        } else {
            assert_eq!(result.0, StatusCode::CONFLICT, "{}", result.2);
            assert_eq!(result.2["reason"], "unique_violation");
            assert!(!result.1.contains_key("entity-revision"));
            assert_eq!(body["fields"]["title"], *original);
            assert_eq!(&response_revision(&headers), baseline);
        }
    }
    let schema = DynSchemaBackend::load_schema_metadata(backend.as_ref(), &other.schema)
        .await
        .unwrap()
        .unwrap();
    let admin_app = app_with_backend(backend.clone(), schema, &["platform_admin"]).await;
    for (index, conditional) in [false, true].into_iter().enumerate() {
        for supplied_owner in [None, Some("attempted-transfer")] {
            let (_, headers, _) =
                request(&admin_app, &other_path, "GET", None, serde_json::json!({})).await;
            let baseline = response_revision(&headers);
            let title = format!("Admin {index} {}", supplied_owner.unwrap_or("omitted"));
            let mut body = serde_json::json!({"title": title});
            if let Some(owner) = supplied_owner {
                body["owner"] = serde_json::json!(owner);
            }
            let (status, headers, response) = request(
                &admin_app,
                &other_path,
                "PUT",
                conditional.then_some(baseline.as_str()),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{response}");
            assert_eq!(response["fields"]["owner"], "other");
            assert!(response["fields"].get("name_key").is_none());
            if conditional {
                assert_ne!(response_revision(&headers), baseline);
            }
            let stored = DynEntityStore::get(backend.as_ref(), &other.schema, &other.id)
                .await
                .unwrap();
            assert_eq!(stored.fields["owner"], DynamicValue::Text("other".into()));
            assert_eq!(
                stored.fields["name_key"],
                DynamicValue::Text(format!("5:other{title}"))
            );
        }
    }
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
        SurrealBackend::connect_memory("conditional_tenant", "conditional_tenant")
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

/// The same route must preserve totals and projection whether storage can
/// certify all rows or a malformed required value forces Cedar scanning.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a scoped disposable PostgreSQL URL and SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1"]
async fn postgres_http_exact_counts_preserve_projection_and_malformed_row_fallback() {
    use schema_forge_acton::state::DynSchemaBackend;
    assert_eq!(
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE").as_deref(),
        Ok("1")
    );
    let url = std::env::var("SCHEMAFORGE_TEST_POSTGRES_URL")
        .expect("SCHEMAFORGE_TEST_POSTGRES_URL must name a disposable namespace");
    let backend = Arc::new(
        schema_forge_postgres::PgBackend::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("could not connect to disposable PostgreSQL")),
    );
    let mut schema = schema_forge_dsl::parse(
        r#"
        @access(read: ["editor"], write: ["editor"], delete: ["editor"])
        schema CountNotice {
            title: text
            secret: text @field_access(read: ["manager"], write: ["manager"])
        }
    "#,
    )
    .unwrap()
    .remove(0);
    backend
        .apply_migration(
            &schema.name,
            &schema_forge_core::migration::DiffEngine::create_new(&schema).steps,
        )
        .await
        .unwrap();
    // Deliberately retain a nullable physical column, as can occur after drift.
    schema.fields[0].modifiers.push(FieldModifier::Required);
    backend.store_schema_metadata(&schema).await.unwrap();
    for title in ["Alpha", "Beta", "Gamma"] {
        DynEntityStore::create(
            backend.as_ref(),
            &Entity::new(
                schema.name.clone(),
                BTreeMap::from([
                    ("title".into(), DynamicValue::Text(title.into())),
                    ("secret".into(), DynamicValue::Text("restricted".into())),
                ]),
            ),
        )
        .await
        .unwrap();
    }
    let app = app_with_backend(backend.clone(), schema.clone(), &["editor"]).await;
    let path = "/schemas/CountNotice/entities?limit=1&offset=1&sort=title&fields=title,secret";
    for malformed in [false, true] {
        if malformed {
            DynEntityStore::create(
                backend.as_ref(),
                &Entity::new(schema.name.clone(), BTreeMap::new()),
            )
            .await
            .unwrap();
        }
        let (status, _, body) = request(&app, path, "GET", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total_count"], 3, "{body}");
        assert_eq!(body["count"], 1, "{body}");
        assert_eq!(body["entities"][0]["fields"]["title"], "Beta", "{body}");
        assert!(
            body["entities"][0]["fields"].get("secret").is_none(),
            "{body}"
        );
    }
}
