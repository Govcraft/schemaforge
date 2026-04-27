//! Cedar authorization entry points.
//!
//! [`authorize`] is the single function every resolver and route handler
//! calls to make an authorization decision. It builds the Cedar request from
//! the supplied [`Claims`] / [`SchemaDefinition`] / optional [`Entity`],
//! evaluates against the current [`PolicyStore`] snapshot, emits an audit
//! event for the decision, and returns an [`AuthzDecision`].
//!
//! [`authorize_field`] is the per-field variant used by response/request
//! field filtering, parameterised by [`FieldDirection`].

use std::sync::Arc;

use acton_service::middleware::Claims;
use cedar_policy::{Authorizer, Context, Decision, Entities, EntityUid, Request};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::SchemaDefinition;

use crate::authz::adapters::{
    action_entity_uid, build_principal_entities, build_resource_entity, principal_uid,
    schema_entity_uid, AdapterError,
};
use crate::authz::namespace::{
    field_read_action_uid, field_write_action_uid, ActionVerb, PRINCIPAL_TYPE,
};
use crate::authz::store::PolicyStore;

/// Direction for field-level authorization decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldDirection {
    /// Reading the field on a response. Maps to `Forge::Action::"ReadField..."`.
    Read,
    /// Writing the field on a request body. Maps to `Forge::Action::"WriteField..."`.
    Write,
}

/// Errors raised while preparing or running a Cedar authorization request.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    /// Domain-to-Cedar adapter failed.
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    /// Cedar request construction failed.
    #[error("invalid Cedar request: {0}")]
    Request(String),
}

/// Result of a single authorization evaluation.
#[derive(Debug, Clone)]
pub struct AuthzDecision {
    /// Whether the request was allowed.
    pub allowed: bool,
    /// Cedar policy IDs that contributed to the decision.
    pub matched_policies: Vec<String>,
    /// Cedar evaluation error messages, if any.
    pub errors: Vec<String>,
}

impl AuthzDecision {
    /// Returns `true` iff the decision was Allow with no evaluation errors.
    pub fn is_allow(&self) -> bool {
        self.allowed && self.errors.is_empty()
    }
}

/// Authorizes a schema-level action against the current policy bundle.
///
/// `resource` may be `None` for actions whose decision depends only on the
/// resource type (e.g., `ListContact`). When `Some`, the entity is converted
/// to a Cedar resource entity carrying every field as a typed attribute, so
/// per-record policies (`@owner`, `@tenant`, custom predicates) can apply.
pub fn authorize(
    store: &Arc<PolicyStore>,
    claims: Option<&Claims>,
    verb: ActionVerb,
    schema: &SchemaDefinition,
    resource: Option<&Entity>,
) -> Result<AuthzDecision, AuthzError> {
    let snapshot = store.current();
    let action = action_entity_uid(verb, schema.name.as_str())?;

    let (principal_uid_value, principal_entities) = match claims {
        Some(c) => {
            let entities = build_principal_entities(c, &snapshot.role_ranks)?;
            let uid = principal_uid(c)?;
            (uid, entities)
        }
        None => {
            // Anonymous principal: a stable synthetic UID with no parent
            // groups and an empty role list. Cedar policies that require a
            // membership predicate will simply not match.
            let raw = format!("{PRINCIPAL_TYPE}::\"_anonymous\"");
            let uid = raw
                .parse::<EntityUid>()
                .map_err(|e| AuthzError::Request(e.to_string()))?;
            (uid, Vec::new())
        }
    };

    let (resource_uid, resource_entities): (EntityUid, Vec<cedar_policy::Entity>) = match resource {
        Some(entity) => {
            let res_entity = build_resource_entity(schema, entity)?;
            let uid = res_entity.uid().clone();
            (uid, vec![res_entity])
        }
        None => (
            schema_entity_uid(schema.name.as_str())?,
            Vec::new(),
        ),
    };

    let mut all_entities: Vec<cedar_policy::Entity> = Vec::new();
    all_entities.extend(principal_entities);
    all_entities.extend(resource_entities);

    let entities = Entities::from_entities(all_entities, Some(&snapshot.schema))
        .map_err(|e| AuthzError::Request(e.to_string()))?;

    let request = Request::new(
        principal_uid_value,
        action,
        resource_uid,
        Context::empty(),
        Some(&snapshot.schema),
    )
    .map_err(|e| AuthzError::Request(e.to_string()))?;

    let response = Authorizer::new().is_authorized(&request, &snapshot.policy_set, &entities);
    let allowed = matches!(response.decision(), Decision::Allow);
    let matched_policies: Vec<String> = response
        .diagnostics()
        .reason()
        .map(|id| id.to_string())
        .collect();
    let errors: Vec<String> = response
        .diagnostics()
        .errors()
        .map(|e| e.to_string())
        .collect();

    let decision = AuthzDecision {
        allowed,
        matched_policies,
        errors,
    };
    audit_decision(claims, verb, schema, resource, &decision);
    Ok(decision)
}

/// Authorizes reading or writing a single field on `entity`.
///
/// The Cedar action evaluated is `Forge::Action::"ReadField{schema}_{field}"`
/// or its `Write` counterpart. Schemas without per-field actions in the
/// generated policy set will trivially Allow (no policy denies, default-permit
/// would normally be unsafe — this engine runs default-deny via a base
/// `forbid` rule generated alongside the per-field permits, so absence of
/// a policy still yields Deny for restricted fields).
pub fn authorize_field(
    store: &Arc<PolicyStore>,
    claims: Option<&Claims>,
    schema: &SchemaDefinition,
    entity: &Entity,
    field_name: &str,
    direction: FieldDirection,
) -> Result<AuthzDecision, AuthzError> {
    let snapshot = store.current();

    let raw_action = match direction {
        FieldDirection::Read => field_read_action_uid(schema.name.as_str(), field_name),
        FieldDirection::Write => field_write_action_uid(schema.name.as_str(), field_name),
    };
    let action: EntityUid = raw_action
        .parse()
        .map_err(|e: cedar_policy::ParseErrors| AuthzError::Request(e.to_string()))?;

    let (principal_uid_value, principal_entities) = match claims {
        Some(c) => {
            let entities = build_principal_entities(c, &snapshot.role_ranks)?;
            let uid = principal_uid(c)?;
            (uid, entities)
        }
        None => {
            let raw = format!("{PRINCIPAL_TYPE}::\"_anonymous\"");
            let uid = raw
                .parse::<EntityUid>()
                .map_err(|e| AuthzError::Request(e.to_string()))?;
            (uid, Vec::new())
        }
    };

    let resource_entity = build_resource_entity(schema, entity)?;
    let resource_uid = resource_entity.uid().clone();
    let mut all_entities = principal_entities;
    all_entities.push(resource_entity);

    let entities = Entities::from_entities(all_entities, Some(&snapshot.schema))
        .map_err(|e| AuthzError::Request(e.to_string()))?;

    // Note: we deliberately skip schema-validating the Request here. Field
    // actions are dynamically generated and may not appear in the schema for
    // every (schema, field) pair — only for fields with @field_access. When
    // the action is absent, Cedar falls through to default-deny via the
    // explicit `forbid` rule the generator emits as a safety net.
    let request = Request::new(
        principal_uid_value,
        action,
        resource_uid,
        Context::empty(),
        None,
    )
    .map_err(|e| AuthzError::Request(e.to_string()))?;

    let response = Authorizer::new().is_authorized(&request, &snapshot.policy_set, &entities);
    let allowed = matches!(response.decision(), Decision::Allow);
    let matched_policies: Vec<String> = response
        .diagnostics()
        .reason()
        .map(|id| id.to_string())
        .collect();
    let errors: Vec<String> = response
        .diagnostics()
        .errors()
        .map(|e| e.to_string())
        .collect();

    let decision = AuthzDecision {
        allowed,
        matched_policies,
        errors,
    };
    let _ = (entity, field_name); // referenced for future audit metadata; logged below via tracing
    let principal_id_field = field_name; // shadow no longer needed; placeholder kept for clarity
    let _ = principal_id_field;
    audit_field_decision(claims, schema, field_name, direction, &decision);
    Ok(decision)
}

fn audit_decision(
    claims: Option<&Claims>,
    verb: ActionVerb,
    schema: &SchemaDefinition,
    resource: Option<&Entity>,
    decision: &AuthzDecision,
) {
    let principal = claims.map(|c| c.sub.as_str()).unwrap_or("_anonymous");
    let resource_id = resource
        .map(|e| e.id.as_str().to_string())
        .unwrap_or_else(|| schema.name.as_str().to_string());
    if decision.allowed {
        tracing::info!(
            target: "schema_forge_acton::authz",
            principal,
            action = verb.as_str(),
            schema = schema.name.as_str(),
            resource = %resource_id,
            matched_policies = ?decision.matched_policies,
            "authz allow"
        );
    } else {
        tracing::warn!(
            target: "schema_forge_acton::authz",
            principal,
            action = verb.as_str(),
            schema = schema.name.as_str(),
            resource = %resource_id,
            errors = ?decision.errors,
            "authz deny"
        );
    }
}

fn audit_field_decision(
    claims: Option<&Claims>,
    schema: &SchemaDefinition,
    field_name: &str,
    direction: FieldDirection,
    decision: &AuthzDecision,
) {
    let principal = claims.map(|c| c.sub.as_str()).unwrap_or("_anonymous");
    let dir = match direction {
        FieldDirection::Read => "read",
        FieldDirection::Write => "write",
    };
    tracing::debug!(
        target: "schema_forge_acton::authz::field",
        principal,
        schema = schema.name.as_str(),
        field = field_name,
        direction = dir,
        allowed = decision.allowed,
        "field-level authz decision"
    );
}
