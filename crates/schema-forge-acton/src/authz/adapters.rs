//! Translation between SchemaForge domain types and Cedar entities.
//!
//! Three responsibilities:
//! 1. Build the Cedar `Principal` entity for the authenticated user from the
//!    JWT [`Claims`]. The principal carries `id`, `role_rank`, `roles`, and
//!    `tenant_chain` attributes plus parent group memberships.
//! 2. Build the Cedar `Resource` entity for a SchemaForge [`Entity`] under a
//!    given [`SchemaDefinition`]. Resource attributes mirror entity field
//!    values, mapped to the Cedar type that the schema declares.
//! 3. Construct the `EntityUid` for an action verb (e.g.,
//!    `Forge::Action::"ReadContact"`).
//!
//! All conversions are pure — they do not consult any global state — so the
//! tests can exercise every edge case (missing fields, anonymous principal,
//! tenant chains of various lengths) without spinning up an authorizer.
//!
//! [`Claims`]: acton_service::middleware::Claims
//! [`Entity`]: schema_forge_backend::entity::Entity
//! [`SchemaDefinition`]: schema_forge_core::types::SchemaDefinition

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use acton_service::middleware::Claims;
use cedar_policy::{Entity as CedarEntity, EntityId, EntityTypeName, EntityUid, RestrictedExpression};
use schema_forge_backend::entity::Entity;
use schema_forge_backend::TenantRef;
use schema_forge_core::types::{Cardinality, DynamicValue, FieldType, SchemaDefinition};

use crate::authz::namespace::{
    action_uid, ActionVerb, GROUP_TYPE, PRINCIPAL_TYPE, SCHEMA_TYPE, TENANT_TYPE,
};
use crate::authz::principal_claims::PrincipalClaimMappings;
use crate::authz::role_ranks::{RoleRanks, PLATFORM_ADMIN_ROLE};

/// Errors raised while converting domain types to Cedar entities.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A Cedar identifier could not be parsed (entity type, action, etc.).
    #[error("invalid Cedar identifier '{value}': {detail}")]
    InvalidIdentifier { value: String, detail: String },
    /// A field value could not be represented as a Cedar restricted expression.
    #[error("field '{field}' carried unrepresentable value: {detail}")]
    UnrepresentableValue { field: String, detail: String },
}

/// Builds the Cedar `EntityUid` for a SchemaForge action.
pub fn action_entity_uid(verb: ActionVerb, schema_name: &str) -> Result<EntityUid, AdapterError> {
    EntityUid::from_str(&action_uid(verb, schema_name)).map_err(|e| AdapterError::InvalidIdentifier {
        value: action_uid(verb, schema_name),
        detail: e.to_string(),
    })
}

/// Builds the Cedar `EntityUid` for a Schema-administration target.
pub fn schema_entity_uid(schema_name: &str) -> Result<EntityUid, AdapterError> {
    let raw = format!("{SCHEMA_TYPE}::\"{schema_name}\"");
    EntityUid::from_str(&raw).map_err(|e| AdapterError::InvalidIdentifier {
        value: raw,
        detail: e.to_string(),
    })
}

/// Returns the principal's stable Cedar UID derived from its claims.
pub fn principal_uid(claims: &Claims) -> Result<EntityUid, AdapterError> {
    let id = user_id_from_sub(&claims.sub);
    let raw = format!("{PRINCIPAL_TYPE}::\"{id}\"");
    EntityUid::from_str(&raw).map_err(|e| AdapterError::InvalidIdentifier {
        value: raw,
        detail: e.to_string(),
    })
}

/// Builds the principal `Entity` plus every group/tenant entity it parents on.
///
/// The returned vector contains the principal first, followed by each unique
/// group and tenant entity referenced by the principal's parents. Adding
/// these entities to a Cedar `Entities` set lets policies say
/// `principal in Forge::Group::"manager"` or
/// `resource._tenant in principal.tenant_ancestors`.
pub fn build_principal_entities(
    claims: &Claims,
    role_ranks: &RoleRanks,
    principal_claims: &PrincipalClaimMappings,
) -> Result<Vec<CedarEntity>, AdapterError> {
    let principal_uid_value = principal_uid(claims)?;
    let id = user_id_from_sub(&claims.sub);

    let role_rank = role_ranks.max_rank(&claims.roles);

    // Principal attributes
    let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
    attrs.insert("id".into(), RestrictedExpression::new_string(id.to_string()));
    attrs.insert("role_rank".into(), RestrictedExpression::new_long(role_rank));
    let role_set: Vec<RestrictedExpression> = claims
        .roles
        .iter()
        .map(|r| RestrictedExpression::new_string(r.clone()))
        .collect();
    attrs.insert(
        "roles".into(),
        RestrictedExpression::new_set(role_set),
    );

    // Operator-supplied principal-claim attributes are populated next.
    // Required-claim absence and type mismatches both surface as
    // `AdapterError::UnrepresentableValue`, which `engine::authorize` maps to
    // an authz error → 401, before any policy is evaluated. Optional claims
    // with no token entry (and no default) are simply omitted; custom Cedar
    // policies must guard them with `principal has X && ...`.
    principal_claims.extract_into(claims, &mut attrs)?;

    // Tenant chain: deepest tenant goes into a `tenant` attribute; the
    // full chain populates the principal's `parents` so policies can
    // express `resource._tenant in principal` for hierarchical scoping.
    let tenant_chain: Vec<TenantRef> = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();

    let mut group_uids: HashSet<EntityUid> = HashSet::new();
    let mut group_entities: Vec<CedarEntity> = Vec::new();
    for role in &claims.roles {
        let raw = format!("{GROUP_TYPE}::\"{role}\"");
        let uid = EntityUid::from_str(&raw).map_err(|e| AdapterError::InvalidIdentifier {
            value: raw.clone(),
            detail: e.to_string(),
        })?;
        if group_uids.insert(uid.clone()) {
            // Each Group entity carries its rank so policies can compare
            // groups by rank if they prefer that to going through the user.
            let rank = role_ranks.get(role).unwrap_or(0);
            let mut group_attrs: HashMap<String, RestrictedExpression> = HashMap::new();
            group_attrs.insert("name".into(), RestrictedExpression::new_string(role.clone()));
            group_attrs.insert("rank".into(), RestrictedExpression::new_long(rank));
            group_entities.push(
                CedarEntity::new(uid, group_attrs, HashSet::new())
                    .map_err(|e| AdapterError::UnrepresentableValue {
                        field: format!("Group::{role}"),
                        detail: e.to_string(),
                    })?,
            );
        }
    }

    // Tenant chain: build one Cedar Tenant entity per hop and parent the
    // principal on every level so policies can express
    // `resource._tenant in principal` for hierarchical scoping. The deepest
    // tenant could be reached as a separate `principal.tenant` attribute,
    // but the Cedar Principal schema doesn't declare such an attribute and
    // no generated policy reads one — so we only populate parents.
    let mut tenant_uids: Vec<EntityUid> = Vec::new();
    let mut tenant_entities: Vec<CedarEntity> = Vec::new();
    for tenant in &tenant_chain {
        let raw = format!("{TENANT_TYPE}::\"{}\"", tenant.entity_id);
        let uid = EntityUid::from_str(&raw).map_err(|e| AdapterError::InvalidIdentifier {
            value: raw.clone(),
            detail: e.to_string(),
        })?;
        tenant_uids.push(uid.clone());
        let mut tenant_attrs: HashMap<String, RestrictedExpression> = HashMap::new();
        tenant_attrs.insert(
            "schema".into(),
            RestrictedExpression::new_string(tenant.schema.clone()),
        );
        tenant_attrs.insert(
            "entity_id".into(),
            RestrictedExpression::new_string(tenant.entity_id.clone()),
        );
        tenant_entities.push(
            CedarEntity::new(uid, tenant_attrs, HashSet::new()).map_err(|e| {
                AdapterError::UnrepresentableValue {
                    field: format!("Tenant::{}", tenant.entity_id),
                    detail: e.to_string(),
                }
            })?,
        );
    }

    let mut parents: HashSet<EntityUid> = HashSet::new();
    parents.extend(group_uids);
    parents.extend(tenant_uids);

    let principal_entity =
        CedarEntity::new(principal_uid_value, attrs, parents).map_err(|e| {
            AdapterError::UnrepresentableValue {
                field: "principal".into(),
                detail: e.to_string(),
            }
        })?;

    let mut all = Vec::with_capacity(1 + group_entities.len() + tenant_entities.len());
    all.push(principal_entity);
    all.extend(group_entities);
    all.extend(tenant_entities);
    Ok(all)
}

/// Builds the Cedar entity representing `entity` under `schema`.
///
/// Field values are mapped to Cedar-typed attributes for use in policy
/// `when`/`unless` clauses. Unknown or non-representable types are skipped
/// silently (Cedar policies that reference them will simply not match).
pub fn build_resource_entity(
    schema: &SchemaDefinition,
    entity: &Entity,
) -> Result<CedarEntity, AdapterError> {
    let raw = format!("{}::\"{}\"", schema.name.as_str(), entity.id.as_str());
    let uid = EntityUid::from_str(&raw).map_err(|e| AdapterError::InvalidIdentifier {
        value: raw,
        detail: e.to_string(),
    })?;

    let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
    for (field_name, value) in &entity.fields {
        // `@hidden` fields are never declared as Cedar attributes, so
        // including them here would fail strict-mode entity validation.
        // The schema's field definition is the canonical source of the
        // hidden flag — entities loaded from storage may still carry the
        // value, but it must not leak into authorization context.
        if let Some(field_def) = schema.field(field_name) {
            if field_def.is_hidden() {
                continue;
            }
        }
        if let Some(expr) = dynamic_to_cedar(value) {
            attrs.insert(field_name.clone(), expr);
        }
    }

    // Resource carries _tenant as a Cedar entity reference when present so
    // tenant policies can do `resource._tenant in principal`.
    if let Some(DynamicValue::Text(tenant_id)) = entity.fields.get("_tenant") {
        let raw_uid = format!("{TENANT_TYPE}::\"{tenant_id}\"");
        if let Ok(t_uid) = EntityUid::from_str(&raw_uid) {
            attrs.insert(
                "_tenant".into(),
                RestrictedExpression::new_entity_uid(t_uid),
            );
        }
    }

    let parents: HashSet<EntityUid> = HashSet::new();
    CedarEntity::new(uid, attrs, parents).map_err(|e| AdapterError::UnrepresentableValue {
        field: format!("resource:{}", schema.name.as_str()),
        detail: e.to_string(),
    })
}

/// Builds a synthetic placeholder Cedar entity for `schema`.
///
/// Used by [`crate::authz::engine::authorize`] when there's no concrete
/// resource yet (e.g. authorising a `CreateContact` or `ListContact` action).
/// The placeholder carries default values for every required field declared
/// in the Cedar schema — `""` for `String`, `0` for `Long`, `false` for
/// `Bool`, an empty set for `Set<...>` — so the strict-mode entity validator
/// accepts it as a member of the declared entity type. Policies that
/// dereference a required attribute will read the default; ownership /
/// tenant guards are designed to fall through when the field doesn't equal
/// the principal's identity, so the placeholder won't accidentally satisfy
/// them.
pub fn build_resource_placeholder(
    schema: &SchemaDefinition,
) -> Result<CedarEntity, AdapterError> {
    let raw = format!("{}::\"_any\"", schema.name.as_str());
    let uid = EntityUid::from_str(&raw).map_err(|e| AdapterError::InvalidIdentifier {
        value: raw,
        detail: e.to_string(),
    })?;

    let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
    for field in &schema.fields {
        if !field.is_required() || field.is_hidden() {
            // Hidden fields are not declared in the Cedar schema, so the
            // strict-mode entity validator would reject a placeholder that
            // includes them.
            continue;
        }
        if let Some(expr) = default_cedar_expr(&field.field_type) {
            attrs.insert(field.name.as_str().to_string(), expr);
        }
    }

    CedarEntity::new(uid, attrs, HashSet::new()).map_err(|e| AdapterError::UnrepresentableValue {
        field: format!("placeholder:{}", schema.name.as_str()),
        detail: e.to_string(),
    })
}

/// Returns a Cedar default expression for a [`FieldType`] used by
/// [`build_resource_placeholder`]. Mirrors the type mapping the Cedar schema
/// generator emits: any `FieldType` that produces a Cedar attribute also
/// produces a default here. Returns `None` for types we don't expose to
/// Cedar (composites, raw JSON), matching the schema generator's output.
fn default_cedar_expr(ft: &FieldType) -> Option<RestrictedExpression> {
    match ft {
        FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_) | FieldType::File(_) => {
            Some(RestrictedExpression::new_string(String::new()))
        }
        FieldType::Integer(_) | FieldType::Float(_) | FieldType::DateTime => {
            Some(RestrictedExpression::new_long(0))
        }
        FieldType::Boolean => Some(RestrictedExpression::new_bool(false)),
        FieldType::Relation { cardinality, .. } => match cardinality {
            Cardinality::One => Some(RestrictedExpression::new_string(String::new())),
            Cardinality::Many => Some(RestrictedExpression::new_set(Vec::new())),
            _ => None,
        },
        FieldType::Array(_) => Some(RestrictedExpression::new_set(Vec::new())),
        _ => None,
    }
}

/// Maps a [`DynamicValue`] to its Cedar `RestrictedExpression` representation.
///
/// Mirrors [`crate::cedar::schema_gen::cedar_type_for`] one-for-one — every
/// `DynamicValue` variant whose `FieldType` advertises a Cedar type produces
/// a corresponding expression here. Mismatches between the schema generator
/// and this function silently drop required attributes and cause strict-mode
/// validation failures, so they must stay in lockstep.
///
/// Returns `None` for variants Cedar cannot represent (composites, raw JSON,
/// `Null`).
pub fn dynamic_to_cedar(value: &DynamicValue) -> Option<RestrictedExpression> {
    match value {
        DynamicValue::Text(s) | DynamicValue::Enum(s) => {
            Some(RestrictedExpression::new_string(s.clone()))
        }
        DynamicValue::Integer(i) => Some(RestrictedExpression::new_long(*i)),
        // Float maps to Long in the Cedar schema; truncate to match. Cedar
        // doesn't have a native float type and policy authors writing
        // numeric predicates over Float fields are already operating in
        // integer space.
        DynamicValue::Float(f) => Some(RestrictedExpression::new_long(*f as i64)),
        DynamicValue::Boolean(b) => Some(RestrictedExpression::new_bool(*b)),
        DynamicValue::DateTime(dt) => {
            Some(RestrictedExpression::new_long(dt.timestamp_millis()))
        }
        DynamicValue::Ref(id) => Some(RestrictedExpression::new_string(id.as_str().to_string())),
        DynamicValue::RefArray(ids) => {
            let items: Vec<RestrictedExpression> = ids
                .iter()
                .map(|id| RestrictedExpression::new_string(id.as_str().to_string()))
                .collect();
            Some(RestrictedExpression::new_set(items))
        }
        DynamicValue::Array(items) => {
            let mapped: Vec<RestrictedExpression> =
                items.iter().filter_map(dynamic_to_cedar).collect();
            Some(RestrictedExpression::new_set(mapped))
        }
        DynamicValue::Null | DynamicValue::Json(_) | DynamicValue::Composite(_) => None,
        // DynamicValue is `#[non_exhaustive]`; new variants default to
        // "no Cedar representation" until explicitly mapped (and a matching
        // Cedar attribute type is added to schema_gen's `cedar_type_for`).
        _ => None,
    }
}

/// Strips the `user:` prefix some auth pipelines add to JWT subjects.
pub fn user_id_from_sub(sub: &str) -> &str {
    sub.strip_prefix("user:").unwrap_or(sub)
}

/// Returns whether the principal carries the `platform_admin` role.
pub fn is_platform_admin(claims: &Claims) -> bool {
    claims.roles.iter().any(|r| r == PLATFORM_ADMIN_ROLE)
}

/// Returns the type name segment for a Cedar entity type (e.g. `Forge::Group`).
pub fn type_name_of(uid: &EntityUid) -> EntityTypeName {
    uid.type_name().clone()
}

/// Returns the id segment of a Cedar entity (e.g. `"alice"`).
pub fn id_of(uid: &EntityUid) -> &EntityId {
    uid.id()
}

#[cfg(test)]
mod principal_claim_tests {
    //! Adapter-level tests for the principal-claim → Cedar attribute path.
    //!
    //! Pure tests covering the contract between [`build_principal_entities`]
    //! and [`PrincipalClaimMappings`] without spinning up the full Cedar
    //! authorizer. Population of intrinsic attributes is already covered by
    //! existing tests elsewhere; these tests focus on the new wiring.

    use super::*;
    use crate::authz::principal_claims::{
        PrincipalClaimConfigEntry, PrincipalClaimMappings, PrincipalClaimType,
        PrincipalClaimsConfig,
    };

    fn claims_with(custom: HashMap<String, serde_json::Value>) -> Claims {
        Claims {
            sub: "user:alice".into(),
            email: None,
            username: None,
            roles: vec!["editor".into()],
            perms: vec![],
            exp: 9_999_999_999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            custom,
        }
    }

    fn mappings(entries: &[(&str, PrincipalClaimType, bool, Option<serde_json::Value>)]) -> PrincipalClaimMappings {
        let mut cfg = PrincipalClaimsConfig::new();
        for (name, t, required, default) in entries {
            cfg.insert(
                (*name).to_string(),
                PrincipalClaimConfigEntry {
                    claim: None,
                    claim_type: *t,
                    required: *required,
                    default: default.clone(),
                    source: None,
                },
            );
        }
        PrincipalClaimMappings::from_config(&cfg).unwrap()
    }

    fn principal_attr_keys(entities: &[CedarEntity]) -> Vec<String> {
        // The principal entity is always the first element by construction.
        let principal = entities
            .iter()
            .find(|e| e.uid().type_name().to_string() == PRINCIPAL_TYPE)
            .expect("principal must be present");
        principal
            .attrs()
            .map(|(k, _v)| k.to_string())
            .collect::<Vec<_>>()
    }

    #[test]
    fn empty_mapping_yields_only_intrinsic_attributes() {
        let claims = claims_with(HashMap::from([(
            "client_org_id".into(),
            serde_json::json!("org-42"),
        )]));
        let entities =
            build_principal_entities(&claims, &RoleRanks::empty(), &PrincipalClaimMappings::default())
                .unwrap();
        let mut keys = principal_attr_keys(&entities);
        keys.sort();
        assert_eq!(keys, vec!["id", "role_rank", "roles"]);
    }

    #[test]
    fn string_claim_populates_attribute_when_present() {
        let m = mappings(&[("client_org_id", PrincipalClaimType::String, false, None)]);
        let claims = claims_with(HashMap::from([(
            "client_org_id".into(),
            serde_json::json!("org-42"),
        )]));
        let entities = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap();
        let mut keys = principal_attr_keys(&entities);
        keys.sort();
        assert_eq!(keys, vec!["client_org_id", "id", "role_rank", "roles"]);
    }

    #[test]
    fn set_of_string_claim_populates_from_array() {
        let m = mappings(&[("team_ids", PrincipalClaimType::SetOfString, false, None)]);
        let claims = claims_with(HashMap::from([(
            "team_ids".into(),
            serde_json::json!(["t-1", "t-2"]),
        )]));
        let entities = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap();
        let keys = principal_attr_keys(&entities);
        assert!(keys.iter().any(|k| k == "team_ids"));
    }

    #[test]
    fn long_claim_populates_from_number() {
        let m = mappings(&[("level", PrincipalClaimType::Long, false, None)]);
        let claims = claims_with(HashMap::from([("level".into(), serde_json::json!(7))]));
        let entities = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap();
        let keys = principal_attr_keys(&entities);
        assert!(keys.iter().any(|k| k == "level"));
    }

    #[test]
    fn required_claim_missing_returns_unrepresentable() {
        let m = mappings(&[("client_org_id", PrincipalClaimType::String, true, None)]);
        let claims = claims_with(HashMap::new());
        let err = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap_err();
        assert!(
            matches!(err, AdapterError::UnrepresentableValue { .. }),
            "expected UnrepresentableValue, got {err:?}"
        );
    }

    #[test]
    fn type_mismatch_returns_unrepresentable() {
        let m = mappings(&[("level", PrincipalClaimType::Long, false, None)]);
        let claims = claims_with(HashMap::from([(
            "level".into(),
            serde_json::json!("not-a-number"),
        )]));
        let err = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap_err();
        assert!(matches!(err, AdapterError::UnrepresentableValue { .. }));
    }

    #[test]
    fn optional_claim_missing_with_default_uses_default() {
        let m = mappings(&[(
            "client_org_id",
            PrincipalClaimType::String,
            false,
            Some(serde_json::json!("fallback-org")),
        )]);
        let claims = claims_with(HashMap::new());
        let entities = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap();
        let keys = principal_attr_keys(&entities);
        assert!(keys.iter().any(|k| k == "client_org_id"));
    }

    #[test]
    fn optional_claim_missing_without_default_is_omitted() {
        let m = mappings(&[("client_org_id", PrincipalClaimType::String, false, None)]);
        let claims = claims_with(HashMap::new());
        let entities = build_principal_entities(&claims, &RoleRanks::empty(), &m).unwrap();
        let keys = principal_attr_keys(&entities);
        assert!(!keys.iter().any(|k| k == "client_org_id"));
    }
}
