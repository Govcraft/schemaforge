//! Spike for issue #50.
//!
//! Question gating the design: does Cedar 4.x strict-mode validation accept a
//! `Forge::Principal` whose schema declares an optional attribute
//! `"client_org_id"?: String`, when policies reference it under a `has` guard
//! and the principal entity at runtime omits the attribute entirely?
//!
//! These tests pin the answer so the implementation can rely on the
//! "skip-when-missing" path described in the issue's proposed design.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use cedar_policy::{
    Authorizer, Context, Decision, Entities, Entity, EntityUid, PolicySet, Request,
    RestrictedExpression, Schema, ValidationMode, Validator,
};

const NS_AND_ACTIONS: &str = r#"
namespace Forge {
    entity Group = {
        name: String,
        rank: Long,
    };

    entity Tenant in [Tenant] = {
        schema: String,
        entity_id: String,
    };

    entity Principal in [Group, Tenant] = {
        id: String,
        role_rank: Long,
        roles: Set<String>,
        "client_org_id"?: String,
    };
}

entity WorkspaceFile in [Forge::Tenant] = {
    "_tenant"?: Forge::Tenant,
    "client_org": String,
};

action ReadWorkspaceFile appliesTo {
    principal: [Forge::Principal],
    resource: [WorkspaceFile],
};
"#;

fn parse_schema(src: &str) -> Schema {
    let (schema, _warnings) = Schema::from_cedarschema_str(src).expect("schema must parse");
    schema
}

fn validate_strict(schema: &Schema, policy_src: &str) -> Result<(), String> {
    let policy_set: PolicySet = policy_src.parse().expect("policy must parse");
    let validator = Validator::new(schema.clone());
    let result = validator.validate(&policy_set, ValidationMode::Strict);
    if result.validation_passed() {
        Ok(())
    } else {
        Err(result
            .validation_errors()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn principal_entity(client_org_id: Option<&str>) -> Entity {
    let uid = EntityUid::from_str(r#"Forge::Principal::"alice""#).unwrap();
    let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
    attrs.insert("id".into(), RestrictedExpression::new_string("alice".into()));
    attrs.insert("role_rank".into(), RestrictedExpression::new_long(50));
    attrs.insert(
        "roles".into(),
        RestrictedExpression::new_set([RestrictedExpression::new_string("editor".into())]),
    );
    if let Some(value) = client_org_id {
        attrs.insert(
            "client_org_id".into(),
            RestrictedExpression::new_string(value.into()),
        );
    }
    Entity::new(uid, attrs, HashSet::new()).expect("principal entity")
}

fn resource_entity(client_org: &str) -> Entity {
    let uid = EntityUid::from_str(&format!(r#"WorkspaceFile::"file-{client_org}""#)).unwrap();
    let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
    attrs.insert(
        "client_org".into(),
        RestrictedExpression::new_string(client_org.into()),
    );
    Entity::new(uid, attrs, HashSet::new()).expect("resource entity")
}

fn read_action_uid() -> EntityUid {
    EntityUid::from_str(r#"Action::"ReadWorkspaceFile""#).unwrap()
}

/// (a) strict-mode validation accepts an optional principal attribute when
///     every policy reference is guarded with `principal has X`.
#[test]
fn strict_validation_accepts_guarded_optional_principal_attribute() {
    let schema = parse_schema(NS_AND_ACTIONS);

    // Realistic shape from the issue: forbid cross-org reads when both sides
    // carry the org claim/field; the `has` guards keep strict mode happy.
    let policy_src = r#"
forbid (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
"#;

    validate_strict(&schema, policy_src).expect("guarded policy must pass strict validation");
}

/// (b) Counter-test: strict-mode validation rejects an unguarded reference to
///     the same optional attribute. This is the safety contract that makes the
///     `has`-guard requirement defensible (operators can't accidentally write
///     a policy that crashes at runtime when the claim is missing).
#[test]
fn strict_validation_rejects_unguarded_optional_principal_attribute() {
    let schema = parse_schema(NS_AND_ACTIONS);

    let policy_src = r#"
forbid (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
)
when {
    resource.client_org != principal.client_org_id
};
"#;

    let err = validate_strict(&schema, policy_src)
        .expect_err("unguarded reference must fail strict validation");
    assert!(
        err.contains("client_org_id") || err.contains("attribute"),
        "expected error referencing the missing attribute, got: {err}"
    );
}

/// (c) Authorizer behavior — principal entity omits the optional attribute,
///     policy guards with `has`. Result: no runtime error; the guarded policy
///     simply does not satisfy and the request is allowed (default deny =
///     no forbid fires; the implicit allow comes from a permit policy below).
#[test]
fn authorizer_allows_when_optional_attribute_absent() {
    let schema = parse_schema(NS_AND_ACTIONS);

    let policy_src = r#"
permit (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
);

forbid (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
"#;
    validate_strict(&schema, policy_src).expect("policy bundle must validate");
    let policy_set: PolicySet = policy_src.parse().unwrap();

    let entities = Entities::from_entities(
        [
            principal_entity(None),
            resource_entity("org-other"),
        ],
        Some(&schema),
    )
    .expect("entities");

    let request = Request::new(
        EntityUid::from_str(r#"Forge::Principal::"alice""#).unwrap(),
        read_action_uid(),
        EntityUid::from_str(r#"WorkspaceFile::"file-org-other""#).unwrap(),
        Context::empty(),
        Some(&schema),
    )
    .expect("request");

    let response = Authorizer::new().is_authorized(&request, &policy_set, &entities);
    assert_eq!(
        response.decision(),
        Decision::Allow,
        "with the principal attribute absent, the forbid must not fire (guarded by `has`)"
    );
}

/// (d) Authorizer behavior — principal entity carries `client_org_id` and the
///     resource's `client_org` differs. The forbid fires.
#[test]
fn authorizer_denies_on_org_mismatch_when_attribute_present() {
    let schema = parse_schema(NS_AND_ACTIONS);

    let policy_src = r#"
permit (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
);

forbid (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
"#;
    let policy_set: PolicySet = policy_src.parse().unwrap();

    let entities = Entities::from_entities(
        [
            principal_entity(Some("org-42")),
            resource_entity("org-other"),
        ],
        Some(&schema),
    )
    .expect("entities");

    let request = Request::new(
        EntityUid::from_str(r#"Forge::Principal::"alice""#).unwrap(),
        read_action_uid(),
        EntityUid::from_str(r#"WorkspaceFile::"file-org-other""#).unwrap(),
        Context::empty(),
        Some(&schema),
    )
    .expect("request");

    let response = Authorizer::new().is_authorized(&request, &policy_set, &entities);
    assert_eq!(response.decision(), Decision::Deny);
}

/// (e) Authorizer behavior — same policies, principal carries `client_org_id`
///     and resource's `client_org` matches. The forbid does not fire; the
///     permit allows.
#[test]
fn authorizer_allows_on_org_match_when_attribute_present() {
    let schema = parse_schema(NS_AND_ACTIONS);

    let policy_src = r#"
permit (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
);

forbid (
    principal,
    action == Action::"ReadWorkspaceFile",
    resource is WorkspaceFile
)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
"#;
    let policy_set: PolicySet = policy_src.parse().unwrap();

    let entities = Entities::from_entities(
        [
            principal_entity(Some("org-42")),
            resource_entity("org-42"),
        ],
        Some(&schema),
    )
    .expect("entities");

    let request = Request::new(
        EntityUid::from_str(r#"Forge::Principal::"alice""#).unwrap(),
        read_action_uid(),
        EntityUid::from_str(r#"WorkspaceFile::"file-org-42""#).unwrap(),
        Context::empty(),
        Some(&schema),
    )
    .expect("request");

    let response = Authorizer::new().is_authorized(&request, &policy_set, &entities);
    assert_eq!(response.decision(), Decision::Allow);
}

/// (f) Sanity: `Entities::from_entities` with the schema validates that an
///     optional attribute is shaped correctly when it IS supplied. (Catches
///     accidental `set_of_string`/scalar drift if the implementation later
///     introduces typed expressions for non-string claims.)
#[test]
fn entities_validation_accepts_well_typed_optional_attribute() {
    let schema = parse_schema(NS_AND_ACTIONS);
    Entities::from_entities([principal_entity(Some("org-7"))], Some(&schema))
        .expect("string-typed optional attribute must be accepted by entity validation");
}

