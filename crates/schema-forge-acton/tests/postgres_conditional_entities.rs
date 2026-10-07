//! Conditional mutation, revision, authorization and projection contracts on PostgreSQL.
use axum::http::StatusCode;
use schema_forge_acton::state::DynEntityStore;
use schema_forge_backend::{conditional::EntityRevision, entity::Entity};
use schema_forge_core::types::{DynamicValue, EntityId, FieldModifier, SchemaName};
use std::{collections::BTreeMap, sync::Arc};

#[path = "support/conditional_entities.rs"]
pub mod conditional_entities;
#[path = "support/postgres.rs"]
mod postgres;
use conditional_entities::{
    app_with_backend, deployment_settings_patch_contract, fixture_with_backend, request,
};

fn response_revision(headers: &axum::http::HeaderMap) -> EntityRevision {
    assert!(!headers.contains_key("etag"));
    assert_eq!(headers["access-control-expose-headers"], "Entity-Revision");
    headers["entity-revision"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a scoped disposable PostgreSQL URL and SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1"]
async fn postgres_http_revisions_guard_updates_noops_races_and_deletes() {
    use schema_forge_acton::state::DynSchemaBackend;
    assert_eq!(
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE").as_deref(),
        Ok("1"),
        "explicit disposable database authorization is required",
    );
    let url = postgres::isolated_url("SCHEMAFORGE_TEST_POSTGRES_HTTP_URL");
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

/// The same route must preserve totals and projection whether storage can
/// certify all rows or a malformed required value forces Cedar scanning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a scoped disposable PostgreSQL URL and SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1"]
async fn postgres_http_exact_counts_preserve_projection_and_malformed_row_fallback() {
    use schema_forge_acton::state::DynSchemaBackend;
    assert_eq!(
        std::env::var("SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE").as_deref(),
        Ok("1")
    );
    let url = postgres::isolated_url("SCHEMAFORGE_TEST_POSTGRES_HTTP_URL");
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
