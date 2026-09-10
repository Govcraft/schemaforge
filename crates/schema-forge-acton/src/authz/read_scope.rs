//! Conservative proof for delegating generated Read authorization to storage.
//!
//! Policy IDs and annotations are not evidence: applicable policy ASTs must
//! exactly match generated rules, and the captured Cedar schema must accept the
//! complete family of row shapes certified by the backend.

use std::collections::{HashMap, HashSet};

use acton_service::middleware::Claims;
use cedar_policy::{
    ActionConstraint, Entities, EntityUid, Policy, PolicySet, ResourceConstraint,
    RestrictedExpression,
};
use schema_forge_backend::{auth::CedarReadScope, TenantRef};
use schema_forge_core::types::{Cardinality, FieldType, SchemaDefinition};

use super::{
    adapters::{action_entity_uid, build_resource_placeholder},
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
    // An inherited action could make a syntactically unrelated policy apply.
    let actions = snapshot.schema.action_entities().ok()?;
    if actions.ancestors(&action)?.next().is_some() {
        return None;
    }
    let source = generate_global_policies(&[])
        .into_iter()
        .chain(generate_cedar_policies(schema))
        .map(|policy| policy.cedar_text)
        .collect::<Vec<_>>()
        .join("\n");
    let expected: PolicySet = source.parse().ok()?;
    let mut expected = applicable_asts(&expected, &action, schema)?;
    let actual = applicable_asts(&snapshot.policy_set, &action, schema)?;
    if actual.len() != expected.len() {
        return None;
    }
    for policy in actual {
        let index = expected.iter().position(|expected| *expected == policy)?;
        expected.swap_remove(index);
    }
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
