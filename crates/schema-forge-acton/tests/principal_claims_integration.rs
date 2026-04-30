//! End-to-end integration test for the principal-claim → Cedar attribute
//! pipeline (issue #50).
//!
//! Boots a `PolicyStoreSnapshot` with a `client_org_id: string` mapping and a
//! tempdir-backed custom Cedar policy that forbids reads when the resource's
//! `client_org` field doesn't match `principal.client_org_id`. Then exercises
//! the authorize() entry point against three scenarios:
//!
//!   1. PASETO `custom.client_org_id == "org-42"`, resource `client_org ==
//!      "org-42"` → Allow (200).
//!   2. Same token, resource `client_org == "org-other"` → Deny (403).
//!   3. Required mapping but token missing the claim → adapter error
//!      (mapped to 401 by the route layer).
//!
//! This is the acceptance gate for the feature: every layer between operator
//! TOML and a Cedar decision is exercised in one test.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use acton_service::middleware::Claims;
use schema_forge_acton::authz::namespace::ActionVerb;
use schema_forge_acton::authz::principal_claims::{
    PrincipalClaimConfigEntry, PrincipalClaimMappings, PrincipalClaimType, PrincipalClaimsConfig,
};
use schema_forge_acton::authz::role_ranks::RoleRanks;
use schema_forge_acton::authz::store::{PolicyStore, PolicyStoreSnapshot};
use schema_forge_acton::authz::{authorize, AuthzError};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Annotation, DynamicValue, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId,
    SchemaName, TextConstraints,
};

const CUSTOM_POLICY: &str = r#"
forbid (
    principal,
    action,
    resource is Document
)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
"#;

fn schema_with_client_org() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("Document").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("client_org").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
        ],
        vec![Annotation::Access {
            read: vec!["member".into()],
            write: vec!["member".into()],
            delete: vec!["member".into()],
            cross_tenant_read: vec![],
        }],
    )
    .unwrap()
}

fn entity_with_org(org: &str) -> Entity {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_string(), DynamicValue::Text("doc".into()));
    fields.insert("client_org".to_string(), DynamicValue::Text(org.into()));
    Entity::new(SchemaName::new("Document").unwrap(), fields)
}

fn mappings_with_client_org(required: bool) -> PrincipalClaimMappings {
    let mut cfg = PrincipalClaimsConfig::new();
    cfg.insert(
        "client_org_id".into(),
        PrincipalClaimConfigEntry {
            claim: None,
            claim_type: PrincipalClaimType::String,
            required,
            default: None,
        },
    );
    PrincipalClaimMappings::from_config(&cfg).unwrap()
}

fn make_claims(custom: HashMap<String, serde_json::Value>) -> Claims {
    Claims {
        sub: "user:alice".into(),
        email: None,
        username: None,
        roles: vec!["member".into()],
        perms: vec![],
        exp: 9_999_999_999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        custom,
    }
}

fn build_store(mappings: PrincipalClaimMappings) -> Arc<PolicyStore> {
    let custom_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(custom_dir.path().join("client_org.cedar"), CUSTOM_POLICY)
        .expect("write policy");

    let snapshot = PolicyStoreSnapshot::from_schemas(
        &[schema_with_client_org()],
        Some(custom_dir.path()),
        RoleRanks::empty(),
        mappings,
    )
    .expect("custom policy + mapping must validate");
    // Hold the tempdir for the lifetime of the test by leaking — `tempdir`
    // is dropped after `from_schemas` reads the file, so leaking is unnecessary
    // here, but kept explicit for clarity.
    drop(custom_dir);
    Arc::new(PolicyStore::new(snapshot))
}

#[test]
fn allows_read_when_org_matches() {
    let store = build_store(mappings_with_client_org(false));
    let claims = make_claims(HashMap::from([(
        "client_org_id".into(),
        serde_json::json!("org-42"),
    )]));
    let schema = schema_with_client_org();
    let resource = entity_with_org("org-42");

    let decision = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &schema,
        Some(&resource),
    )
    .expect("authz must not error when claim matches");

    assert!(
        decision.is_allow(),
        "expected Allow, got matched={:?} errors={:?}",
        decision.matched_policies,
        decision.errors,
    );
}

#[test]
fn denies_read_when_org_does_not_match() {
    let store = build_store(mappings_with_client_org(false));
    let claims = make_claims(HashMap::from([(
        "client_org_id".into(),
        serde_json::json!("org-42"),
    )]));
    let schema = schema_with_client_org();
    let resource = entity_with_org("org-other");

    let decision = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &schema,
        Some(&resource),
    )
    .expect("authz must reach a decision");

    assert!(
        !decision.is_allow(),
        "expected Deny when resource.client_org != principal.client_org_id, \
         got Allow with matched={:?}",
        decision.matched_policies,
    );
}

#[test]
fn required_mapping_with_missing_claim_returns_adapter_error() {
    // When the operator declares the claim required, a token missing it must
    // never reach the Cedar evaluator — the adapter returns an error that the
    // route layer maps to a 401. This is the integrity gate for required
    // claims: no policy can compensate for missing input.
    let store = build_store(mappings_with_client_org(true));
    let claims = make_claims(HashMap::new()); // no client_org_id in custom
    let schema = schema_with_client_org();
    let resource = entity_with_org("org-42");

    let err = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &schema,
        Some(&resource),
    )
    .expect_err("required-claim absence must surface as an authz error");

    match err {
        AuthzError::Adapter(_) => {}
        other => panic!("expected Adapter error, got {other:?}"),
    }
}

#[test]
fn optional_mapping_with_missing_claim_proceeds_to_evaluation() {
    // Symmetric to the required case: when the mapping is optional and the
    // token omits the claim, the adapter just skips populating the attribute
    // and Cedar evaluates as normal. The has-guarded forbid does not fire,
    // so the request reaches the access-driven permit and gets through.
    let store = build_store(mappings_with_client_org(false));
    let claims = make_claims(HashMap::new());
    let schema = schema_with_client_org();
    let resource = entity_with_org("org-42");

    let decision = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &schema,
        Some(&resource),
    )
    .expect("authz must reach a decision");

    assert!(
        decision.is_allow(),
        "expected Allow when claim is optional and absent, got matched={:?} errors={:?}",
        decision.matched_policies,
        decision.errors,
    );
}
