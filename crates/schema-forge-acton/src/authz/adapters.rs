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
use schema_forge_core::types::{DynamicValue, SchemaDefinition};

use crate::authz::namespace::{
    action_uid, ActionVerb, GROUP_TYPE, PRINCIPAL_TYPE, SCHEMA_TYPE, TENANT_TYPE,
};
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
        RestrictedExpression::new_set(role_set.into_iter()),
    );

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

    if let Some(deepest) = tenant_uids.last().cloned() {
        attrs.insert(
            "tenant".into(),
            RestrictedExpression::new_entity_uid(deepest),
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

/// Maps a [`DynamicValue`] to its Cedar `RestrictedExpression` representation.
///
/// Returns `None` for variants Cedar cannot represent (e.g., embedded
/// composites with arbitrary nesting).
pub fn dynamic_to_cedar(value: &DynamicValue) -> Option<RestrictedExpression> {
    match value {
        DynamicValue::Text(s) => Some(RestrictedExpression::new_string(s.clone())),
        DynamicValue::Integer(i) => Some(RestrictedExpression::new_long(*i)),
        DynamicValue::Boolean(b) => Some(RestrictedExpression::new_bool(*b)),
        DynamicValue::Ref(id) => Some(RestrictedExpression::new_string(id.as_str().to_string())),
        DynamicValue::Null => None,
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
