//! External-identity uniqueness and authorization contracts on SurrealDB.
use std::sync::Arc;

use schema_forge_core::system_schemas;

#[path = "support/oauth_storage.rs"]
pub mod oauth_storage;
use oauth_storage::exercise;

#[tokio::test]
async fn surreal_identity_pair_is_unique() {
    exercise(Arc::new(
        schema_forge_surrealdb::test_support::connect("oauth_storage", "oauth_storage")
            .await
            .unwrap(),
    ))
    .await;
}

#[test]
fn identity_write_guard_overrides_broad_permits_for_non_admins() {
    use schema_forge_acton::authz::{
        authorize, namespace::ActionVerb, PolicyStore, PolicyStoreSnapshot, PrincipalClaimMappings,
        RoleRanks,
    };
    let schemas: Vec<_> = [
        system_schemas::USER_SCHEMA,
        system_schemas::OAUTH_IDENTITY_SCHEMA,
    ]
    .into_iter()
    .map(|text| schema_forge_dsl::parse(text).unwrap().remove(0))
    .collect();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("broad.cedar"),
        "permit(principal, action, resource);",
    )
    .unwrap();
    let store = Arc::new(PolicyStore::new(
        PolicyStoreSnapshot::from_schemas(
            &schemas,
            Some(directory.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap(),
    ));
    for admin in [false, true] {
        let caller: acton_service::middleware::Claims = serde_json::from_value(serde_json::json!({
            "sub": "user:alice", "roles": if admin { vec!["platform_admin"] } else { vec!["viewer"] }, "perms": [], "exp": 9999999999_u64,
        })).unwrap();
        for verb in [ActionVerb::Create, ActionVerb::Update, ActionVerb::Delete] {
            let decision = authorize(&store, Some(&caller), verb, &schemas[1], None).unwrap();
            assert_eq!(
                decision.is_allow(),
                admin,
                "unexpected decision for {verb:?}, admin={admin}: {decision:?}"
            );
        }
    }
}
