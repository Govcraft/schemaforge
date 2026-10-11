//! Conservative proof for delegating generated Read authorization to storage.
//!
//! Policy IDs and annotations are not evidence: applicable policy ASTs must
//! exactly match generated rules for Read-scope delegation. Field predicates may
//! additionally use custom rules proven record-independent for the caller. The
//! captured Cedar schema must accept every certified concrete row shape.

use std::collections::{HashMap, HashSet};

use acton_service::middleware::Claims;
use cedar_policy::{
    ActionConstraint, Authorizer, Context, Decision, Entities, EntityUid, Policy, PolicySet,
    PrincipalConstraint, Request, ResourceConstraint, RestrictedExpression,
};
use schema_forge_backend::{auth::CedarReadScope, TenantRef};
use schema_forge_core::types::{Cardinality, FieldType, SchemaDefinition};

use super::{
    adapters::{
        action_entity_uid, build_principal_entities, build_query_resource_placeholder,
        build_resource_placeholder, principal_uid,
    },
    namespace::ActionVerb,
    PolicyStoreSnapshot,
};
use crate::cedar::policy_gen::{generate_cedar_policies, generate_global_policies};

/// Only succeeds after the prepared principal and schema preflight are allowed.
pub(crate) fn generated_read_scope(
    snapshot: &PolicyStoreSnapshot,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
) -> Option<CedarReadScope> {
    // The adapters parse quoted Cedar UIDs. Reject escapes rather than compare
    // SQL membership against a different identity from the parsed Cedar one.
    if claims.is_some_and(|claims| {
        claims.roles.iter().any(|role| !canonical_identity(role))
            || claims
                .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
                .unwrap_or_default()
                .iter()
                .any(|tenant| !canonical_identity(&tenant.entity_id))
    }) {
        return None;
    }
    let action = action_entity_uid(ActionVerb::Read, schema.name.as_str()).ok()?;
    matches_generated_policies(snapshot, schema, &action)?;
    if !compatible_resource_shape(snapshot, schema) {
        return None;
    }
    if claims.is_some_and(|claims| claims.roles.iter().any(|role| role == "platform_admin")) {
        Some(CedarReadScope::Unrestricted)
    } else {
        let tenants = claims
            .and_then(|claims| claims.custom_claim_as::<Vec<TenantRef>>("tenant_chain"))
            .unwrap_or_default()
            .into_iter()
            .map(|tenant| tenant.entity_id)
            .collect();
        Some(CedarReadScope::TenantMembers(tenants))
    }
}

/// Certify field reads for this principal without inspecting synthetic record
/// attributes. Generated rules must remain intact; additional matching policies
/// must have record-invariant scopes and supported record-independent conditions.
pub(crate) fn field_read_is_record_independent(
    snapshot: &PolicyStoreSnapshot,
    schema: &SchemaDefinition,
    field_name: &str,
    claims: Option<&Claims>,
    tenant: Option<&str>,
) -> bool {
    tenant.is_none_or(canonical_identity)
        && compatible_resource_shape(snapshot, schema)
        && field_policy_independence_proof(snapshot, schema, field_name, claims, tenant).is_some()
}

fn field_policy_independence_proof(
    snapshot: &PolicyStoreSnapshot,
    schema: &SchemaDefinition,
    field_name: &str,
    claims: Option<&Claims>,
    tenant: Option<&str>,
) -> Option<()> {
    let action = super::namespace::field_read_action_uid(schema.name.as_str(), field_name)
        .parse::<EntityUid>()
        .ok()?;
    let actions = snapshot.schema.action_entities().ok()?;
    if actions.ancestors(&action)?.next().is_some() {
        return None;
    }
    let (principal, entities) = match claims {
        Some(claims) => (
            principal_uid(claims).ok()?,
            build_principal_entities(claims, &snapshot.role_ranks, &snapshot.principal_claims)
                .ok()?,
        ),
        None => (
            format!("{}::\"_anonymous\"", super::namespace::PRINCIPAL_TYPE)
                .parse::<EntityUid>()
                .ok()?,
            Vec::new(),
        ),
    };
    let mut entities = entities;
    let resource = build_query_resource_placeholder(schema, tenant).ok()?;
    let resource_uid = resource.uid();
    entities.push(resource);
    let entities = Entities::from_entities(entities, Some(&snapshot.schema)).ok()?;
    let request = Request::new(
        principal.clone(),
        action.clone(),
        resource_uid,
        Context::from_pairs([(
            "resource_is_placeholder".into(),
            RestrictedExpression::new_bool(true),
        )])
        .ok()?,
        Some(&snapshot.schema),
    )
    .ok()?;
    let condition_context = ConditionContext {
        request,
        entities,
        tenant_bound: tenant.is_some(),
    };
    let generated = generated_policies(schema)?;
    let mut expected = applicable_asts(&generated, &action, schema)?;
    for policy in snapshot
        .policy_set
        .policies()
        .filter(|policy| can_apply(policy, &action, schema))
    {
        let ast = policy.to_json().ok()?;
        if let Some(index) = expected.iter().position(|expected| *expected == ast) {
            expected.swap_remove(index);
            continue;
        }
        if principal_constraint_matches(
            &policy.principal_constraint(),
            &principal,
            &condition_context.entities,
        ) && !custom_field_policy_is_constant(policy, &ast, &condition_context)
        {
            return None;
        }
    }
    expected.is_empty().then_some(())
}

fn principal_constraint_matches(
    constraint: &PrincipalConstraint,
    principal: &EntityUid,
    entities: &Entities,
) -> bool {
    match constraint {
        PrincipalConstraint::Any => true,
        PrincipalConstraint::Eq(uid) => uid == principal,
        // Cedar `in` is reflexive. Entities::is_ancestor_of only checks
        // stored ancestors for a present entity in Cedar 4.9.1.
        PrincipalConstraint::In(uid) => uid == principal || entities.is_ancestor_of(uid, principal),
        PrincipalConstraint::Is(kind) => principal.type_name() == kind,
        PrincipalConstraint::IsIn(kind, uid) => {
            principal.type_name() == kind
                && (uid == principal || entities.is_ancestor_of(uid, principal))
        }
    }
}

fn custom_field_policy_is_constant(
    policy: &Policy,
    ast: &serde_json::Value,
    context: &ConditionContext,
) -> bool {
    matches!(
        policy.resource_constraint(),
        ResourceConstraint::Any | ResourceConstraint::Is(_)
    ) && ast
        .get("conditions")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().all(|condition| {
                matches!(
                    condition.get("kind").and_then(serde_json::Value::as_str),
                    Some("when" | "unless")
                ) && condition.get("body").is_some_and(|body| {
                    matches!(
                        prove_condition(body, context, context.tenant_bound),
                        Some(ConditionTruth::Known(_))
                    )
                })
            })
        })
}

struct ConditionContext {
    request: Request,
    entities: Entities,
    tenant_bound: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConditionTruth {
    Known(bool),
    /// A total Boolean, whose value may vary between unscoped tenants.
    Unknown,
}

/// Evaluate only expressions certified independent, or tenant expressions whose
/// presence is bound by enforcement. A single diagnostic error invalidates proof.
fn evaluate_boolean(expression: &serde_json::Value, context: &ConditionContext) -> Option<bool> {
    let policy = Policy::from_json(
        None,
        serde_json::json!({
            "effect": "permit", "principal": {"op": "All"}, "action": {"op": "All"},
            "resource": {"op": "All"}, "conditions": [{"kind": "when", "body": expression}]
        }),
    )
    .ok()?;
    let mut policies = PolicySet::new();
    policies.add(policy).ok()?;
    let response = Authorizer::new().is_authorized(&context.request, &policies, &context.entities);
    if response.diagnostics().errors().next().is_some() {
        return None;
    }
    Some(response.decision() == Decision::Allow)
}

fn tenant_attribute(expression: &serde_json::Value, operator: &str) -> bool {
    expression.get(operator).is_some_and(|operands| {
        operands.get("left") == Some(&serde_json::json!({"Var": "resource"}))
            && operands.get("attr").and_then(serde_json::Value::as_str) == Some("_tenant")
    })
}

fn guarantees_tenant_presence(expression: &serde_json::Value) -> bool {
    tenant_attribute(expression, "has")
        || expression.get("&&").is_some_and(|operands| {
            ["left", "right"]
                .iter()
                .any(|key| operands.get(key).is_some_and(guarantees_tenant_presence))
        })
}

/// Ordered, totality-aware Boolean reasoning. `error && false` is an error in
/// Cedar; only total left operands can be absorbed by a constant right operand.
fn prove_condition(
    expression: &serde_json::Value,
    context: &ConditionContext,
    tenant_present: bool,
) -> Option<ConditionTruth> {
    if record_independent_expression(expression) {
        return evaluate_boolean(expression, context).map(ConditionTruth::Known);
    }
    if tenant_attribute(expression, "has") {
        return Some(if tenant_present {
            ConditionTruth::Known(true)
        } else {
            ConditionTruth::Unknown
        });
    }
    let object = expression.as_object().filter(|object| object.len() == 1)?;
    let (operator, operands) = object.iter().next()?;
    let left = || operands.get("left");
    let right = || operands.get("right");
    match operator.as_str() {
        "==" | "!=" => {
            let (left, right) = (left()?, right()?);
            let tenant_and_literal = |tenant, literal: &serde_json::Value| {
                tenant_attribute(tenant, ".")
                    && literal
                        .get("Value")
                        .and_then(|value| value.get("__entity"))
                        .is_some()
            };
            if !tenant_present
                || !(tenant_and_literal(left, right) || tenant_and_literal(right, left))
            {
                return None;
            }
            if context.tenant_bound {
                evaluate_boolean(expression, context).map(ConditionTruth::Known)
            } else {
                Some(ConditionTruth::Unknown)
            }
        }
        "&&" | "||" => {
            let (left, right) = (left()?, right()?);
            let left_truth = prove_condition(left, context, tenant_present)?;
            let conjunction = operator == "&&";
            if left_truth == ConditionTruth::Known(!conjunction) {
                return Some(left_truth);
            }
            let right_truth = prove_condition(
                right,
                context,
                tenant_present || (conjunction && guarantees_tenant_presence(left)),
            )?;
            match (left_truth, right_truth) {
                (ConditionTruth::Known(_), right) => Some(right),
                (_, ConditionTruth::Known(value)) if value != conjunction => {
                    Some(ConditionTruth::Known(value))
                }
                _ => Some(ConditionTruth::Unknown),
            }
        }
        "!" => match prove_condition(operands.get("arg")?, context, tenant_present)? {
            ConditionTruth::Known(value) => Some(ConditionTruth::Known(!value)),
            ConditionTruth::Unknown => Some(ConditionTruth::Unknown),
        },
        "if-then-else" => {
            let condition = operands.get("if")?;
            match prove_condition(condition, context, tenant_present)? {
                ConditionTruth::Known(value) => prove_condition(
                    operands.get(if value { "then" } else { "else" })?,
                    context,
                    tenant_present || (value && guarantees_tenant_presence(condition)),
                ),
                ConditionTruth::Unknown => {
                    let then_truth = prove_condition(
                        operands.get("then")?,
                        context,
                        tenant_present || guarantees_tenant_presence(condition),
                    )?;
                    let else_truth =
                        prove_condition(operands.get("else")?, context, tenant_present)?;
                    Some(if then_truth == else_truth {
                        then_truth
                    } else {
                        ConditionTruth::Unknown
                    })
                }
            }
        }
        _ => None,
    }
}

/// A closed whitelist of Cedar EST forms. Never search policy text: strings,
/// annotations, and attribute names are not variable references. Direct caller
/// attributes are fixed by the trusted adapter; other dereferences could alias
/// the current resource even when they contain no `resource` variable.
fn record_independent_expression(expression: &serde_json::Value) -> bool {
    let Some(object) = expression.as_object().filter(|object| object.len() == 1) else {
        return false;
    };
    let Some((operator, operands)) = object.iter().next() else {
        return false;
    };
    let independent = |key| operands.get(key).is_some_and(record_independent_expression);
    match operator.as_str() {
        // Cedar EST Value nodes are literal values, never variable expressions.
        "Value" => true,
        "Var" => matches!(operands.as_str(), Some("principal" | "action")),
        "." | "has" => {
            operands.get("left") == Some(&serde_json::json!({"Var": "principal"}))
                && operands
                    .get("attr")
                    .is_some_and(serde_json::Value::is_string)
        }
        "!" | "neg" | "isEmpty" => independent("arg"),
        "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||" | "+" | "-" | "*" | "contains"
        | "containsAll" | "containsAny" => independent("left") && independent("right"),
        "in" => {
            // Membership consults entity ancestry. Only the known principal or
            // action can be its subject; arbitrary entity literals could alias
            // a concrete resource supplied by a later authorization call.
            operands
                .get("left")
                .and_then(|left| left.get("Var"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|variable| matches!(variable, "principal" | "action"))
                && independent("left")
                && independent("right")
        }
        "if-then-else" => independent("if") && independent("then") && independent("else"),
        "Set" => operands
            .as_array()
            .is_some_and(|values| values.iter().all(record_independent_expression)),
        "Record" => operands
            .as_object()
            .is_some_and(|values| values.values().all(record_independent_expression)),
        "like" => independent("left"),
        // Includes resource/context variables, slots, nested/dynamic entity
        // attributes, tags, extensions, and future forms we have not proved.
        _ => false,
    }
}

fn generated_policies(schema: &SchemaDefinition) -> Option<PolicySet> {
    generate_global_policies(&[])
        .into_iter()
        .chain(generate_cedar_policies(schema))
        .map(|policy| policy.cedar_text)
        .collect::<Vec<_>>()
        .join("\n")
        .parse()
        .ok()
}

fn matches_generated_policies(
    snapshot: &PolicyStoreSnapshot,
    schema: &SchemaDefinition,
    action: &EntityUid,
) -> Option<()> {
    // An inherited action could make a syntactically unrelated policy apply.
    let actions = snapshot.schema.action_entities().ok()?;
    if actions.ancestors(action)?.next().is_some() {
        return None;
    }
    let expected = generated_policies(schema)?;
    let mut expected = applicable_asts(&expected, action, schema)?;
    let actual = applicable_asts(&snapshot.policy_set, action, schema)?;
    if actual.len() != expected.len() {
        return None;
    }
    for policy in actual {
        let index = expected.iter().position(|expected| *expected == policy)?;
        expected.swap_remove(index);
    }
    Some(())
}

fn canonical_identity(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_:-".contains(&byte))
}

fn applicable_asts(
    set: &PolicySet,
    action: &EntityUid,
    schema: &SchemaDefinition,
) -> Option<Vec<serde_json::Value>> {
    set.policies()
        .filter(|policy| can_apply(policy, action, schema))
        .map(|policy| policy.to_json().ok())
        .collect()
}

pub(crate) fn can_apply(policy: &Policy, action: &EntityUid, schema: &SchemaDefinition) -> bool {
    let action_matches = match policy.action_constraint() {
        ActionConstraint::Any => true,
        ActionConstraint::Eq(uid) => uid == *action,
        ActionConstraint::In(uids) => uids.contains(action),
    };
    action_matches
        && match policy.resource_constraint() {
            ResourceConstraint::Is(kind) | ResourceConstraint::IsIn(kind, _) => {
                kind.to_string() == schema.name.as_str()
            }
            ResourceConstraint::Eq(uid) => uid.type_name().to_string() == schema.name.as_str(),
            ResourceConstraint::Any | ResourceConstraint::In(_) => true,
        }
}

/// Required-only and fully populated typed witnesses establish all possible
/// nullable attribute combinations. Nonempty sets prove their element types;
/// empty sets alone would incorrectly certify a stale array declaration.
fn compatible_resource_shape(snapshot: &PolicyStoreSnapshot, schema: &SchemaDefinition) -> bool {
    let Ok(minimal) = build_resource_placeholder(schema) else {
        return false;
    };
    if Entities::from_entities([minimal.clone()], Some(&snapshot.schema)).is_err() {
        return false;
    }
    let mut attributes = HashMap::new();
    attributes.insert(
        "id".into(),
        RestrictedExpression::new_string("_shape_witness".into()),
    );
    for field in &schema.fields {
        if field.is_hidden() {
            continue;
        }
        match witness_expression(&field.field_type) {
            Some(Some(expression)) => {
                attributes.insert(field.name.to_string(), expression);
            }
            Some(None) => {}
            None => return false,
        }
    }
    let Ok(tenant) = "Forge::Tenant::\"_shape_witness\"".parse::<EntityUid>() else {
        return false;
    };
    attributes.insert(
        "_tenant".into(),
        RestrictedExpression::new_entity_uid(tenant),
    );
    let Ok(full) = cedar_policy::Entity::new(minimal.uid().clone(), attributes, HashSet::new())
    else {
        return false;
    };
    Entities::from_entities([full], Some(&snapshot.schema)).is_ok()
}

/// Outer None means unsupported, inner None means intentionally absent in Cedar.
fn witness_expression(field_type: &FieldType) -> Option<Option<RestrictedExpression>> {
    let expression = match field_type {
        FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_) => {
            RestrictedExpression::new_string("sample".into())
        }
        FieldType::Integer(_) | FieldType::Float(_) | FieldType::DateTime => {
            RestrictedExpression::new_long(1)
        }
        FieldType::Boolean => RestrictedExpression::new_bool(true),
        FieldType::Relation {
            cardinality: Cardinality::One,
            ..
        } => RestrictedExpression::new_string("related_sample".into()),
        FieldType::Array(inner) => RestrictedExpression::new_set([witness_expression(inner)??]),
        FieldType::Json => return Some(None),
        _ => return None,
    };
    Some(Some(expression))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{
        engine::PreparedAuthorization, PolicyStore, PrincipalClaimMappings, RoleRanks,
    };
    use schema_forge_backend::entity::Entity;
    use schema_forge_core::types::{DynamicValue, EntityId};
    use std::{collections::BTreeMap, sync::Arc};

    fn claims(role: &str) -> Claims {
        Claims {
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
            custom: HashMap::new(),
        }
    }

    fn schema(fields: &str) -> SchemaDefinition {
        schema_forge_dsl::parse(&format!(
            r#"
            @access(read: ["reader"], write: ["editor"], delete: ["editor"])
            schema Notice {{ {fields} }}
        "#
        ))
        .unwrap()
        .remove(0)
    }

    fn snapshot(schema: &SchemaDefinition, custom: &str) -> PolicyStoreSnapshot {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("custom.cedar"), custom).unwrap();
        PolicyStoreSnapshot::from_schemas(
            std::slice::from_ref(schema),
            Some(directory.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap()
    }

    #[test]
    fn generated_read_scope_preserves_tenant_guard_and_admin_exception() {
        let schema = schema("title: text required tags: text[] enabled: boolean details: json");
        let snapshot = snapshot(&schema, "");
        let mut reader = claims("reader");
        reader.custom.insert(
            "tenant_chain".into(),
            serde_json::json!([
                {"schema":"Organization", "entity_id":"organization_alpha"},
                {"schema":"Team", "entity_id":"team_beta"}
            ]),
        );
        assert!(
            matches!(generated_read_scope(&snapshot, &schema, Some(&reader)),
            Some(CedarReadScope::TenantMembers(ids)) if ids == ["organization_alpha", "team_beta"])
        );
        assert!(matches!(
            generated_read_scope(&snapshot, &schema, Some(&claims("platform_admin"))),
            Some(CedarReadScope::Unrestricted)
        ));
    }

    #[test]
    fn custom_uid_context_and_attribute_policies_cannot_use_generated_proof() {
        let schema = schema("title: text required enabled: boolean");
        for custom in [
            r#"forbid(principal, action == Action::"ReadNotice", resource == Notice::"notice_hidden");"#,
            r#"forbid(principal, action == Action::"ReadNotice", resource is Notice) when { !context.resource_is_placeholder };"#,
            r#"forbid(principal, action == Action::"ReadNotice", resource is Notice) when { resource has enabled && !resource.enabled };"#,
        ] {
            assert!(generated_read_scope(
                &snapshot(&schema, custom),
                &schema,
                Some(&claims("reader"))
            )
            .is_none());
        }
    }

    #[test]
    fn escaped_tenant_or_role_identity_requires_cedar_evaluation() {
        let schema = schema("title: text required");
        let snapshot = snapshot(&schema, "");
        let mut reader = claims("reader");
        reader.custom.insert(
            "tenant_chain".into(),
            serde_json::json!([
                {"schema":"Team", "entity_id":r"\u0061"}
            ]),
        );
        assert!(generated_read_scope(&snapshot, &schema, Some(&reader)).is_none());
        assert!(
            generated_read_scope(&snapshot, &schema, Some(&claims(r"platform_\u0061dmin")))
                .is_none()
        );
    }

    #[test]
    fn unrelated_custom_write_does_not_disable_generated_read_proof() {
        let schema = schema("title: text required");
        let snapshot = snapshot(
            &schema,
            r#"forbid(principal, action == Action::"UpdateNotice", resource is Notice);"#,
        );
        assert!(generated_read_scope(&snapshot, &schema, Some(&claims("reader"))).is_some());
    }

    #[test]
    fn field_read_proof_requires_concrete_shape_compatibility() {
        let current = schema(r#"title: text @field_access(read: ["reader"]) enabled: boolean"#);
        let stale = schema(r#"title: text @field_access(read: ["reader"]) enabled: integer"#);
        assert!(field_read_is_record_independent(
            &snapshot(&current, ""),
            &current,
            "title",
            Some(&claims("reader")),
            None
        ));
        // Placeholder omits enabled, so a stale optional type could otherwise
        // allow the predicate while concrete response projection denies it.
        assert!(!field_read_is_record_independent(
            &snapshot(&stale, ""),
            &current,
            "title",
            Some(&claims("reader")),
            None
        ));
    }

    #[test]
    fn replacing_generated_field_rule_with_safe_custom_rule_cannot_pass_proof() {
        let schema = schema(r#"title: text @field_access(read: ["reader"])"#);
        let mut snapshot = snapshot(&schema, "");
        let action = super::super::namespace::field_read_action_uid("Notice", "title")
            .parse::<EntityUid>()
            .unwrap();
        let generated_id = snapshot
            .policy_set
            .policies()
            .find(|policy| policy.action_constraint() == ActionConstraint::Eq(action.clone()))
            .unwrap()
            .id()
            .clone();
        snapshot
            .policy_set
            .remove_static(generated_id.clone())
            .unwrap();
        // Reusing the generated ID and its @id annotation is not a proof that
        // the generated role contract remains intact.
        let replacement = Policy::parse(
            Some(generated_id),
            r#"
            @id("forge.notice.field_title_read_reader")
            permit(principal, action == Action::"ReadFieldNotice_title", resource is Notice)
            when { principal.role_rank >= 0 };
        "#,
        )
        .unwrap();
        snapshot.policy_set.add(replacement).unwrap();
        assert!(!field_read_is_record_independent(
            &snapshot,
            &schema,
            "title",
            Some(&claims("reader")),
            None
        ));
    }

    fn test_condition_context(tenant: Option<&str>) -> ConditionContext {
        test_condition_context_for_role(tenant, "reader")
    }

    fn test_condition_context_for_role(tenant: Option<&str>, role: &str) -> ConditionContext {
        let schema = schema("title: text required @field_access(read: [\"reader\"])");
        let snapshot = snapshot(&schema, "");
        let reader = claims(role);
        let resource = build_query_resource_placeholder(&schema, tenant).unwrap();
        let request = Request::new(
            principal_uid(&reader).unwrap(),
            super::super::namespace::field_read_action_uid("Notice", "title")
                .parse::<EntityUid>()
                .unwrap(),
            resource.uid(),
            Context::from_pairs([(
                "resource_is_placeholder".into(),
                RestrictedExpression::new_bool(true),
            )])
            .unwrap(),
            Some(&snapshot.schema),
        )
        .unwrap();
        let mut entities =
            build_principal_entities(&reader, &snapshot.role_ranks, &snapshot.principal_claims)
                .unwrap();
        entities.push(resource);
        ConditionContext {
            request,
            entities: Entities::from_entities(entities, Some(&snapshot.schema)).unwrap(),
            tenant_bound: tenant.is_some(),
        }
    }

    #[test]
    fn custom_expression_proof_rejects_aliases_context_and_unknown_forms() {
        for condition in [
            r#"principal.link.title == "secret""#,
            r#"principal.link has title"#,
            r#"Notice::"some_record" has title"#,
            r#"Notice::"some_record".title == "secret""#,
            r#"Notice::"some_record" in principal"#,
            r#"if principal.role_rank > 0 then true else resource has title"#,
            r#"principal.role_rank > 0 || !context.resource_is_placeholder"#,
        ] {
            let policy: Policy =
                format!("forbid(principal, action, resource) when {{ {condition} }};")
                    .parse()
                    .unwrap();
            assert!(
                !custom_field_policy_is_constant(
                    &policy,
                    &policy.to_json().unwrap(),
                    &test_condition_context(None)
                ),
                "{condition}"
            );
        }
        assert!(!record_independent_expression(&serde_json::json!({
            "futureOperator": {"arg": {"Var": "principal"}}
        })));
    }

    #[test]
    fn custom_expression_proof_distinguishes_literals_from_variable_references() {
        let policy: Policy = r#"
            @id("resource.context.resource_is_placeholder")
            forbid(principal, action, resource is Notice)
            when { principal.id == "resource" || principal.roles.contains("context") };
        "#
        .parse()
        .unwrap();
        assert!(custom_field_policy_is_constant(
            &policy,
            &policy.to_json().unwrap(),
            &test_condition_context(None)
        ));
    }

    #[test]
    fn tenant_boolean_proof_preserves_error_order_and_positive_presence_guards() {
        let context = test_condition_context(None);
        for (condition, expected) in [
            (
                r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared" && false"#,
                true,
            ),
            (
                r#"resource._tenant == Forge::Tenant::"shared" && false"#,
                false,
            ),
            (
                r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared" && principal.missing == "x" && false"#,
                false,
            ),
            (r#"principal.missing == "x" && false"#, false),
            (r#"false && principal.missing == "x""#, true),
            (
                r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared" && false && principal.missing == "x""#,
                true,
            ),
            (
                r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared" || true"#,
                true,
            ),
            (
                r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared""#,
                false,
            ),
        ] {
            let policy: Policy =
                format!("forbid(principal, action, resource) when {{ {condition} }};")
                    .parse()
                    .unwrap();
            assert_eq!(
                custom_field_policy_is_constant(&policy, &policy.to_json().unwrap(), &context),
                expected,
                "{condition}"
            );
        }
    }

    #[test]
    fn accepted_boolean_proofs_agree_with_cedar_across_the_unscoped_tenant_domain() {
        let conditions = [
            r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared" && !(principal in Forge::Group::"platform_admin")"#,
            r#"resource has _tenant && resource._tenant == Forge::Tenant::"shared" || principal in Forge::Group::"platform_admin""#,
            r#"!(resource has _tenant && resource._tenant == Forge::Tenant::"shared" && principal in Forge::Group::"platform_admin")"#,
            r#"if resource has _tenant then resource._tenant == Forge::Tenant::"shared" && false else false"#,
            r#"if resource has _tenant && resource._tenant == Forge::Tenant::"shared" then principal in Forge::Group::"platform_admin" else principal in Forge::Group::"platform_admin""#,
            r#"if principal in Forge::Group::"platform_admin" then false else resource has _tenant && resource._tenant == Forge::Tenant::"shared" && false"#,
        ];
        let mut certified = 0;
        for role in ["reader", "platform_admin"] {
            for condition in conditions {
                let policy: Policy =
                    format!("permit(principal, action, resource) when {{ {condition} }};")
                        .parse()
                        .unwrap();
                let ast = policy.to_json().unwrap();
                let body = &ast["conditions"][0]["body"];
                if let Some(ConditionTruth::Known(expected)) =
                    prove_condition(body, &test_condition_context_for_role(None, role), false)
                {
                    certified += 1;
                    for tenant in [None, Some("shared"), Some("allowed"), Some("another")] {
                        assert_eq!(
                            evaluate_boolean(body, &test_condition_context_for_role(tenant, role)),
                            Some(expected),
                            "{role}, {tenant:?}, {condition}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            certified, 9,
            "the table must exercise accepted proofs, not vacuously skip every case"
        );
    }

    #[test]
    fn tenant_bound_proof_rejects_identity_translation_and_certifies_all_tenant_values() {
        let schema = schema("title: text required @field_access(read: [\"reader\"])");
        let snapshot = snapshot(
            &schema,
            r#"forbid(principal, action, resource) when { resource has _tenant && resource._tenant == Forge::Tenant::"shared" };"#,
        );
        for tenant in ["shared", "allowed"] {
            assert!(field_read_is_record_independent(
                &snapshot,
                &schema,
                "title",
                Some(&claims("reader")),
                Some(tenant)
            ));
        }
        for tenant in ["", "bad\\nidentity", "bad\"identity"] {
            assert!(!field_read_is_record_independent(
                &snapshot,
                &schema,
                "title",
                Some(&claims("reader")),
                Some(tenant)
            ));
        }
        assert!(!field_read_is_record_independent(
            &snapshot,
            &schema,
            "title",
            Some(&claims("reader")),
            None
        ));
    }

    #[test]
    fn stale_requiredness_and_nonempty_array_types_fail_schema_proof() {
        let current = schema("title: text tags: text[]");
        for stale in [
            schema("title: text required tags: text[]"),
            schema("title: text tags: integer[]"),
        ] {
            assert!(
                generated_read_scope(&snapshot(&stale, ""), &current, Some(&claims("reader")))
                    .is_none()
            );
        }
    }

    #[test]
    fn replacing_tenant_guard_with_duplicate_permit_cannot_pass_ast_comparison() {
        let schema = schema("title: text required");
        let mut snapshot = snapshot(&schema, "");
        let guard_id = snapshot
            .policy_set
            .policies()
            .find(|policy| policy.to_string().contains("tenant_guard"))
            .unwrap()
            .id()
            .clone();
        snapshot.policy_set.remove_static(guard_id).unwrap();
        let duplicate = snapshot
            .policy_set
            .policies()
            .next()
            .unwrap()
            .new_id("duplicate_generated_policy".parse().unwrap());
        snapshot.policy_set.add(duplicate).unwrap();
        assert!(generated_read_scope(&snapshot, &schema, Some(&claims("reader"))).is_none());
    }

    #[test]
    fn prepared_evaluation_retains_snapshot_and_real_row_denials() {
        let schema = schema("title: text required enabled: boolean");
        let reader = claims("reader");
        let store = Arc::new(PolicyStore::new(snapshot(
            &schema,
            r#"
            forbid(principal, action == Action::"ReadNotice", resource is Notice)
            when { resource has enabled && !resource.enabled };
        "#,
        )));
        let prepared =
            PreparedAuthorization::new(store.current(), Some(&reader), ActionVerb::Read, &schema)
                .unwrap();
        store.swap(snapshot(&schema, ""));
        let entity = Entity::with_id(
            EntityId::new("notice"),
            schema.name.clone(),
            BTreeMap::from([
                ("title".into(), DynamicValue::Text("Sample".into())),
                ("enabled".into(), DynamicValue::Boolean(false)),
            ]),
        );
        assert!(!prepared.authorize(Some(&entity)).unwrap().is_allow());
        assert!(crate::authz::authorize(
            &store,
            Some(&reader),
            ActionVerb::Read,
            &schema,
            Some(&entity)
        )
        .unwrap()
        .is_allow());
        let malformed = Entity::with_id(
            EntityId::new("notice"),
            schema.name.clone(),
            BTreeMap::new(),
        );
        assert!(prepared.authorize(Some(&malformed)).is_err());
    }
}
