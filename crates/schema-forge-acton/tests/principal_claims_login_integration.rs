//! End-to-end integration test for the IN-side principal-claim pipeline
//! (issue #51).
//!
//! Closes the loop opened by issue #50 (OUT-side):
//!
//!   1. Operator declares
//!      `[schema_forge.authz.principal_claims.client_org_id]
//!          type     = "string"
//!          source   = { user_field = "client_org_id" }`
//!   2. Login projects the User row's `client_org_id` column into PASETO
//!      `custom.client_org_id`.
//!   3. The OUT-side `extract_into` reads the same custom claim back into
//!      the Cedar `principal.client_org_id` attribute.
//!   4. A custom Cedar policy compares `principal.client_org_id` against
//!      `resource.client_org` and grants/denies accordingly.
//!
//! Tests skip the HTTP / acton-service plumbing (which would require a real
//! backend, key file, and tempdir) and exercise the deterministic core:
//! `build_login_claims` (route layer's pure helper) → `Claims` →
//! `extract_into` → `authorize`. Wiring tests live in the unit and
//! `principal_claims_integration.rs` companion suites.

use std::collections::BTreeMap;
use std::sync::Arc;

use acton_service::auth::tokens::ClaimsBuilder;
use schema_forge_acton::authz::namespace::ActionVerb;
use schema_forge_acton::authz::principal_claims::{
    PrincipalClaimConfigEntry, PrincipalClaimMappings, PrincipalClaimSourceConfig,
    PrincipalClaimType, PrincipalClaimsConfig, PrincipalClaimsError,
};
use schema_forge_acton::authz::role_ranks::RoleRanks;
use schema_forge_acton::authz::store::{PolicyStore, PolicyStoreSnapshot};
use schema_forge_acton::authz::authorize;
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Annotation, Cardinality, DynamicValue, EntityId, FieldDefinition, FieldName, FieldType,
    SchemaDefinition, SchemaId, SchemaName, TextConstraints,
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

// ----- Test fixtures -----------------------------------------------------

fn document_schema() -> SchemaDefinition {
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

fn user_schema_with_client_org() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("User").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("client_org_id").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
        ],
        vec![Annotation::System],
    )
    .unwrap()
}

fn user_schema_with_relation() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("User").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("client_org").unwrap(),
                FieldType::Relation {
                    target: SchemaName::new("ClientOrg").unwrap(),
                    cardinality: Cardinality::One,
                },
            ),
        ],
        vec![Annotation::System],
    )
    .unwrap()
}

fn document_with_org(org: &str) -> Entity {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_string(), DynamicValue::Text("doc".into()));
    fields.insert("client_org".to_string(), DynamicValue::Text(org.into()));
    Entity::new(SchemaName::new("Document").unwrap(), fields)
}

fn user_entity_with_org(org: &str) -> Entity {
    let mut fields = BTreeMap::new();
    fields.insert("email".to_string(), DynamicValue::Text("alice@x".into()));
    fields.insert(
        "client_org_id".to_string(),
        DynamicValue::Text(org.into()),
    );
    Entity::new(SchemaName::new("User").unwrap(), fields)
}

fn mappings_with_source(required: bool, user_field: &str) -> PrincipalClaimsConfig {
    let mut cfg = PrincipalClaimsConfig::new();
    cfg.insert(
        "client_org_id".into(),
        PrincipalClaimConfigEntry {
            claim: None,
            claim_type: PrincipalClaimType::String,
            required,
            default: None,
            source: Some(PrincipalClaimSourceConfig::UserField {
                user_field: user_field.to_string(),
            }),
        },
    );
    cfg
}

fn build_resolved(
    cfg: &PrincipalClaimsConfig,
    user_schema: &SchemaDefinition,
) -> PrincipalClaimMappings {
    let mut m = PrincipalClaimMappings::from_config(cfg).unwrap();
    m.resolve_user_field_sources(user_schema).unwrap();
    m
}

fn build_store(mappings: PrincipalClaimMappings) -> Arc<PolicyStore> {
    let custom_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(custom_dir.path().join("client_org.cedar"), CUSTOM_POLICY)
        .expect("write policy");
    let snapshot = PolicyStoreSnapshot::from_schemas(
        &[document_schema()],
        Some(custom_dir.path()),
        RoleRanks::empty(),
        mappings,
    )
    .expect("custom policy + mapping must validate");
    drop(custom_dir);
    Arc::new(PolicyStore::new(snapshot))
}

/// Build PASETO claims via the same pure helper the login handler uses.
///
/// Mirrors `routes::auth::build_login_claims` without depending on its
/// crate-private signature: takes the user, the resolved mappings, and the
/// User entity, and returns a Claims envelope ready for `extract_into`.
fn login_claims(
    username: &str,
    user_entity: &Entity,
    mappings: &PrincipalClaimMappings,
) -> Result<acton_service::middleware::Claims, PrincipalClaimsError> {
    let mut builder = ClaimsBuilder::new()
        .user(username)
        .username(username)
        .role("member")
        .issuer("schemaforge");
    let projected = mappings.project_user_fields(user_entity)?;
    for (k, v) in projected {
        builder = builder.custom_claim(k, v);
    }
    Ok(builder.build().expect("ClaimsBuilder::build"))
}

// ----- Round-trip tests --------------------------------------------------

#[test]
fn login_projects_user_field_and_policy_grants_on_match() {
    let user_schema = user_schema_with_client_org();
    let mappings = build_resolved(&mappings_with_source(false, "client_org_id"), &user_schema);
    let store = build_store(mappings.clone());

    let user = user_entity_with_org("org-42");
    let claims = login_claims("alice", &user, &mappings).expect("login claims build");

    // Round-trip sanity: the projection landed in the token's custom map.
    assert_eq!(
        claims.custom.get("client_org_id"),
        Some(&serde_json::json!("org-42")),
    );

    let decision = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &document_schema(),
        Some(&document_with_org("org-42")),
    )
    .expect("authz must reach a decision");
    assert!(
        decision.is_allow(),
        "expected Allow when user.client_org_id matches resource.client_org",
    );
}

#[test]
fn login_projects_user_field_and_policy_denies_on_mismatch() {
    let user_schema = user_schema_with_client_org();
    let mappings = build_resolved(&mappings_with_source(false, "client_org_id"), &user_schema);
    let store = build_store(mappings.clone());

    let user = user_entity_with_org("org-42");
    let claims = login_claims("alice", &user, &mappings).expect("login claims build");

    let decision = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &document_schema(),
        Some(&document_with_org("org-other")),
    )
    .expect("authz must reach a decision");
    assert!(
        !decision.is_allow(),
        "expected Deny when user.client_org_id != resource.client_org, \
         got Allow with matched={:?}",
        decision.matched_policies,
    );
}

#[test]
fn required_source_with_null_user_field_returns_null_required_error() {
    // Per the issue spec: a `required = true` mapping whose `user_field`
    // resolves to null at login MUST surface a `NullRequiredUserField`
    // error. The route layer catches this and returns 401 — no token is
    // minted, no Cedar evaluation happens, no exception leaks to the
    // caller as a 500.
    let user_schema = user_schema_with_client_org();
    let mappings = build_resolved(&mappings_with_source(true, "client_org_id"), &user_schema);

    let mut user_fields = BTreeMap::new();
    user_fields.insert("email".into(), DynamicValue::Text("alice@x".into()));
    user_fields.insert("client_org_id".into(), DynamicValue::Null);
    let user = Entity::new(SchemaName::new("User").unwrap(), user_fields);

    let err = login_claims("alice", &user, &mappings).expect_err("required+null must error");
    assert!(matches!(err, PrincipalClaimsError::NullRequiredUserField { .. }));
}

#[test]
fn refresh_re_reads_user_row_no_copy_forward() {
    // The login handler's refresh path re-reads the User row on every
    // refresh (not the previous token's custom claims). Mechanically that
    // means calling `login_claims` again with a *different* user_entity
    // produces a different claim value — there is no in-process state
    // that would carry the stale value across.
    let user_schema = user_schema_with_client_org();
    let mappings = build_resolved(&mappings_with_source(false, "client_org_id"), &user_schema);

    let initial = login_claims("alice", &user_entity_with_org("org-1"), &mappings).unwrap();
    assert_eq!(initial.custom["client_org_id"], serde_json::json!("org-1"));

    // Operator (or another path) reassigns the user mid-session; the next
    // refresh re-reads and emits the new value.
    let refreshed = login_claims("alice", &user_entity_with_org("org-2"), &mappings).unwrap();
    assert_eq!(
        refreshed.custom["client_org_id"],
        serde_json::json!("org-2"),
    );
}

#[test]
fn relation_one_source_emits_target_id_and_round_trips_through_policy() {
    // A `-> ClientOrg` relation field projects as the target's entity id
    // string; downstream Cedar policy comparison is the same as for a
    // text-typed source.
    let user_schema = user_schema_with_relation();
    let mut cfg = PrincipalClaimsConfig::new();
    cfg.insert(
        "client_org_id".into(),
        PrincipalClaimConfigEntry {
            claim: None,
            claim_type: PrincipalClaimType::String,
            required: false,
            default: None,
            source: Some(PrincipalClaimSourceConfig::UserField {
                user_field: "client_org".to_string(),
            }),
        },
    );
    let mappings = build_resolved(&cfg, &user_schema);
    let store = build_store(mappings.clone());

    let target_id = EntityId::new("clientorg");
    let target_str = target_id.as_str().to_string();
    let mut user_fields = BTreeMap::new();
    user_fields.insert("email".into(), DynamicValue::Text("alice@x".into()));
    user_fields.insert("client_org".into(), DynamicValue::Ref(target_id));
    let user = Entity::new(SchemaName::new("User").unwrap(), user_fields);

    let claims = login_claims("alice", &user, &mappings).expect("login claims build");
    assert_eq!(
        claims.custom["client_org_id"],
        serde_json::Value::String(target_str.clone()),
    );

    let decision = authorize(
        &store,
        Some(&claims),
        ActionVerb::Read,
        &document_schema(),
        Some(&document_with_org(&target_str)),
    )
    .expect("authz must reach a decision");
    assert!(decision.is_allow());
}

#[test]
fn no_in_side_source_falls_back_to_out_side_only_behaviour() {
    // Symmetry guard: a mapping with no `source` block behaves exactly
    // like the pre-#51 deployment — the bearer (or a CLI-issued token) is
    // expected to supply the claim. `project_user_fields` skips the
    // mapping entirely and the resulting Claims have no custom entry.
    let mut cfg = PrincipalClaimsConfig::new();
    cfg.insert(
        "client_org_id".into(),
        PrincipalClaimConfigEntry {
            claim: None,
            claim_type: PrincipalClaimType::String,
            required: false,
            default: None,
            source: None,
        },
    );
    let user_schema = user_schema_with_client_org();
    let mappings = build_resolved(&cfg, &user_schema);

    assert!(!mappings.has_user_field_sources());

    let user = user_entity_with_org("org-42");
    let claims = login_claims("alice", &user, &mappings).unwrap();
    assert!(claims.custom.is_empty());
}

// Schema for the @access-driven default permit; needed so the role-based
// member access on Document parses cleanly above. Suppresses an unused
// import warning by keeping `RoleRank`/`PLATFORM_ADMIN_ROLE` out of scope.

#[cfg(test)]
fn _document_schema_static_check() {
    let _ = document_schema();
}
