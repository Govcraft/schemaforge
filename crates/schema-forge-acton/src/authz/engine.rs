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

use std::error::Error as StdError;
use std::sync::Arc;

use acton_service::middleware::Claims;
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityUid, Request, RestrictedExpression,
};
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::SchemaDefinition;

use crate::authz::adapters::{
    action_entity_uid, build_principal_entities, build_resource_entity, build_resource_placeholder,
    principal_uid, AdapterError,
};
use crate::authz::namespace::{
    field_read_action_uid, field_write_action_uid, ActionVerb, PRINCIPAL_TYPE,
};
use crate::authz::store::PolicyStore;

/// Renders an error plus its full source chain.
///
/// Cedar wraps `EntitySchemaConformanceError` behind `#[diagnostic(transparent)]`
/// so the outer `Display` is a generic "entity does not conform to the schema"
/// — the actually-useful detail (which entity, which attribute) lives in the
/// source chain. Operators triaging an authz failure need that detail; without
/// it every conformance bug looks identical.
fn render_error_chain<E: StdError + ?Sized>(err: &E) -> String {
    let mut out = err.to_string();
    let mut src = err.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

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
    /// Cedar's raw Allow result. Use [`Self::is_allow`] for the effective
    /// decision, which also rejects evaluation errors.
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

/// Context is derived by the engine, never from claims or resource fields.
fn authorization_context(resource_is_placeholder: bool) -> Result<Context, AuthzError> {
    Context::from_pairs([(
        "resource_is_placeholder".into(),
        RestrictedExpression::new_bool(resource_is_placeholder),
    )])
    .map_err(|error| AuthzError::Request(render_error_chain(&error)))
}

/// Authorizes a schema-level action against the current policy bundle.
///
/// `resource` may be `None` for actions whose decision depends only on the
/// resource type (e.g., `ListContact`). When `Some`, the entity is converted
/// to a Cedar resource entity carrying every field as a typed attribute, so
/// per-record policies (`@owner`, `@tenant`, custom predicates) can apply.
/// `context.resource_is_placeholder` is true for `None` and false for `Some`.
/// Schema checks include Read preflights for both lists and point reads.
pub fn authorize(
    store: &Arc<PolicyStore>,
    claims: Option<&Claims>,
    verb: ActionVerb,
    schema: &SchemaDefinition,
    resource: Option<&Entity>,
) -> Result<AuthzDecision, AuthzError> {
    PreparedAuthorization::new(store.current(), claims, verb, schema)?.authorize(resource)
}

/// Request-local immutable authorization state, reused across candidate rows.
pub(crate) struct PreparedAuthorization<'a> {
    pub(crate) snapshot: Arc<crate::authz::PolicyStoreSnapshot>,
    claims: Option<&'a Claims>,
    verb: ActionVerb,
    schema: &'a SchemaDefinition,
    principal_uid: EntityUid,
    action: EntityUid,
    entities: Entities,
    authorizer: Authorizer,
    policies: cedar_policy::PolicySet,
}

impl<'a> PreparedAuthorization<'a> {
    pub(crate) fn new(
        snapshot: Arc<crate::authz::PolicyStoreSnapshot>,
        claims: Option<&'a Claims>,
        verb: ActionVerb,
        schema: &'a SchemaDefinition,
    ) -> Result<Self, AuthzError> {
        let action = action_entity_uid(verb, schema.name.as_str())?;

        let (principal_uid_value, principal_entities) = match claims {
            Some(c) => {
                let entities =
                    build_principal_entities(c, &snapshot.role_ranks, &snapshot.principal_claims)?;
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
                    .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;
                (uid, Vec::new())
            }
        };

        let entities = Entities::from_entities(principal_entities, Some(&snapshot.schema))
            .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;
        // Policy heads alone can exclude other actions/resource types without
        // evaluating or dropping any applicable custom condition.
        let policies = relevant_policies(&snapshot, &action, schema)
            .unwrap_or_else(|| snapshot.policy_set.clone());
        Ok(Self {
            policies,
            snapshot,
            claims,
            verb,
            schema,
            principal_uid: principal_uid_value,
            action,
            entities,
            authorizer: Authorizer::new(),
        })
    }

    pub(crate) fn authorize(&self, resource: Option<&Entity>) -> Result<AuthzDecision, AuthzError> {
        let (resource_uid, resource_entities): (EntityUid, Vec<cedar_policy::Entity>) =
            match resource {
                Some(entity) => {
                    let res_entity = build_resource_entity(self.schema, entity)?;
                    let uid = res_entity.uid().clone();
                    (uid, vec![res_entity])
                }
                None => {
                    // Schema-level checks (no specific resource yet — e.g. authorising
                    // a `CreateX` request before the entity exists) need a placeholder
                    // resource of the correct app-schema entity type so per-action
                    // `appliesTo` declarations match and the strict-mode entity
                    // validator accepts the entity. The placeholder is populated with
                    // synthetic default values for every required field — policies
                    // that inspect attributes will see the defaults. The explicit
                    // context marker lets policies distinguish this preflight from a
                    // concrete resource without inferring scope from attribute values.
                    let placeholder = build_resource_placeholder(self.schema)?;
                    let uid = placeholder.uid().clone();
                    (uid, vec![placeholder])
                }
            };

        let entities = self
            .entities
            .clone()
            .add_entities(resource_entities, Some(&self.snapshot.schema))
            .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;

        let request = Request::new(
            self.principal_uid.clone(),
            self.action.clone(),
            resource_uid.clone(),
            authorization_context(resource.is_none())?,
            Some(&self.snapshot.schema),
        )
        .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;

        let response = self
            .authorizer
            .is_authorized(&request, &self.policies, &entities);
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
        audit_decision(
            self.claims,
            self.verb,
            self.schema,
            resource,
            &resource_uid,
            &decision,
        );
        Ok(decision)
    }
}

fn relevant_policies(
    snapshot: &crate::authz::PolicyStoreSnapshot,
    action: &EntityUid,
    schema: &SchemaDefinition,
) -> Option<cedar_policy::PolicySet> {
    if snapshot
        .schema
        .action_entities()
        .ok()?
        .ancestors(action)?
        .next()
        .is_some()
    {
        return None;
    }
    let mut policies = cedar_policy::PolicySet::new();
    for policy in snapshot.policy_set.policies() {
        if crate::authz::read_scope::can_apply(policy, action, schema) {
            policies.add(policy.clone()).ok()?;
        }
    }
    Some(policies)
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
            let entities =
                build_principal_entities(c, &snapshot.role_ranks, &snapshot.principal_claims)?;
            let uid = principal_uid(c)?;
            (uid, entities)
        }
        None => {
            let raw = format!("{PRINCIPAL_TYPE}::\"_anonymous\"");
            let uid = raw
                .parse::<EntityUid>()
                .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;
            (uid, Vec::new())
        }
    };

    let resource_entity = build_resource_entity(schema, entity)?;
    let resource_uid = resource_entity.uid().clone();
    let mut all_entities = principal_entities;
    all_entities.push(resource_entity);

    let entities = Entities::from_entities(all_entities, Some(&snapshot.schema))
        .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;

    // `filter_entity_fields` only calls `authorize_field` for fields whose
    // schema declares a `@field_access` annotation, so the per-field action
    // is guaranteed to appear in the Cedar schema. That makes strict request
    // validation safe here — and surfaces real validation errors instead of
    // silently default-denying when something has drifted.
    let request = Request::new(
        principal_uid_value,
        action,
        resource_uid,
        authorization_context(false)?,
        Some(&snapshot.schema),
    )
    .map_err(|e| AuthzError::Request(render_error_chain(&e)))?;

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
    resource_uid: &EntityUid,
    decision: &AuthzDecision,
) {
    let principal = claims.map(|c| c.sub.as_str()).unwrap_or("_anonymous");
    let resource_id = resource
        .map(|e| e.id.as_str().to_string())
        .unwrap_or_else(|| schema.name.as_str().to_string());
    if decision.is_allow() {
        tracing::info!(
            target: "schema_forge_acton::authz",
            principal,
            action = verb.as_str(),
            schema = schema.name.as_str(),
            resource = %resource_id,
            resource_uid = %resource_uid,
            resource_is_placeholder = resource.is_none(),
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
            resource_uid = %resource_uid,
            resource_is_placeholder = resource.is_none(),
            matched_policies = ?decision.matched_policies,
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
        resource_is_placeholder = false,
        allowed = decision.is_allow(),
        "field-level authz decision"
    );
}
