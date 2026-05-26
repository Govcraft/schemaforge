use std::collections::BTreeMap;
use std::sync::Arc;

use acton_service::middleware::Claims;
use schema_forge_backend::entity::Entity;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_backend::TenantRef;
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{DynamicValue, SchemaDefinition};

use serde::Serialize;

use crate::authz::adapters::user_id_from_sub;
use crate::authz::namespace::ActionVerb;
use crate::authz::{authorize, authorize_field, FieldDirection, PolicyStore};
use crate::error::ForgeError;

/// Actions that can be checked against schema-level `@access` annotations.
///
/// The Cedar engine maps each variant to one or more action UIDs:
/// `Read` → `Action::"Read{X}"` and `Action::"List{X}"`;
/// `Write` → `Action::"Update{X}"` (Create is checked at the route layer
///   via [`AccessAction::Create`] for endpoints that distinguish);
/// `Delete` → `Action::"Delete{X}"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessAction {
    /// Reading entities (GET single, GET list).
    Read,
    /// Listing entities (GET collection). Mapped separately from `Read` so
    /// authors can grant list-only access without per-entity reads.
    List,
    /// Creating new entities (POST).
    Create,
    /// Updating existing entities (PUT/PATCH).
    Update,
    /// Backwards-compatible alias accepted by the schema-level check; maps
    /// to the union of `Create` and `Update` (caller is permitted if they
    /// can do either).
    Write,
    /// Deleting entities (DELETE).
    Delete,
}

impl AccessAction {
    fn primary_verb(self) -> ActionVerb {
        match self {
            Self::Read => ActionVerb::Read,
            Self::List => ActionVerb::List,
            Self::Create | Self::Write => ActionVerb::Create,
            Self::Update => ActionVerb::Update,
            Self::Delete => ActionVerb::Delete,
        }
    }

    /// Returns the secondary verbs that must also pass for this action.
    ///
    /// `Write` is the only variant with a secondary; it succeeds if either
    /// `Create` OR `Update` is permitted.
    fn additional_verbs(self) -> &'static [ActionVerb] {
        match self {
            Self::Write => &[ActionVerb::Update],
            _ => &[],
        }
    }
}

/// Direction for field-level access filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldFilterDirection {
    /// Filter fields for read operations (GET responses).
    Read,
    /// Filter fields for write operations (POST/PUT request bodies).
    Write,
}

/// Extractor that optionally extracts `Claims` from request extensions.
///
/// Required because axum's `Extension<T>` rejects the request if `T`
/// is not present. Since claims may not be present (e.g., unauthenticated
/// requests), we need a custom extractor that returns `None` when no
/// `Claims` exists.
pub struct OptionalClaims(pub Option<Claims>);

impl<S> axum::extract::FromRequestParts<S> for OptionalClaims
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(OptionalClaims(parts.extensions.get::<Claims>().cloned()))
    }
}

/// The reserved role name that grants unauthenticated (public) access.
///
/// When a schema's `@access` annotation includes `"public"` in a role list,
/// that action is accessible without any `Claims`.
pub const PUBLIC_ROLE: &str = "public";

/// Dedicated platform-superuser role.
///
/// The only role that bypasses schema/field/tenant access checks and gates
/// the user-management endpoints (`/api/v1/forge/users`). Distinct from any
/// in-app role string an application chooses (including `admin`), so
/// applications are free to use `admin` as their highest in-app tier
/// without inheriting platform-wide privileges.
///
/// Re-exported from `schema_forge_backend` so the value lives in exactly
/// one place — the lower crate where `OwnershipBasedPolicy` also reads it.
pub use schema_forge_backend::PLATFORM_ADMIN_ROLE;

/// Check whether the authenticated user is permitted to perform `action` on
/// `schema`, delegating the decision to the Cedar policy engine.
///
/// Returns `Ok(())` on Allow, `Err(ForgeError::Unauthorized)` when no
/// claims are present, and `Err(ForgeError::Forbidden)` on Deny. Cedar
/// is the single source of truth: this function is a thin shim that
/// builds the request and translates the engine's decision into a
/// `ForgeError`.
pub fn check_schema_access(
    store: &Arc<PolicyStore>,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    action: AccessAction,
) -> Result<(), ForgeError> {
    if claims.is_none() {
        return Err(ForgeError::Unauthorized {
            message: "authentication required".to_string(),
        });
    }

    let primary = action.primary_verb();
    let primary_decision = authorize(store, claims, primary, schema, None).map_err(|e| {
        ForgeError::Internal {
            message: format!("authz engine error: {e}"),
        }
    })?;
    if primary_decision.is_allow() {
        return Ok(());
    }

    // For `Write`, also try the alternate verb (Create OR Update suffices).
    for &verb in action.additional_verbs() {
        let alt = authorize(store, claims, verb, schema, None).map_err(|e| ForgeError::Internal {
            message: format!("authz engine error: {e}"),
        })?;
        if alt.is_allow() {
            return Ok(());
        }
    }

    Err(ForgeError::Forbidden {
        message: format!(
            "access denied: user lacks permission for {:?} on schema '{}'",
            action,
            schema.name.as_str(),
        ),
    })
}

/// Summary of schema-level operations the caller may perform.
///
/// Computed server-side from the live Cedar bundle so the client can render
/// action affordances (e.g., "New" buttons) without guessing. `read`/`list`
/// is implicit: callers without read access never see the response.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SchemaPermissions {
    /// Whether the caller can create new entities of this schema.
    pub create: bool,
}

/// Summary of per-entity operations the caller may perform.
///
/// Computed server-side by evaluating Cedar against the entity's actual
/// attributes, so per-record policies (`@owner`, `@tenant`, custom
/// predicates) get factored in without the client second-guessing them.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct EntityPermissions {
    /// Whether the caller can update this entity.
    pub update: bool,
    /// Whether the caller can delete this entity.
    pub delete: bool,
}

/// Compute schema-level permissions for the caller.
///
/// Cedar's `Create{Schema}` action is evaluated against a placeholder
/// resource (Cedar requires *some* resource entity even for create) — that
/// matches what `routes::entities::create_entity` does at request time, so
/// this preview is honest by construction.
pub fn schema_permissions(
    store: &Arc<PolicyStore>,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
) -> SchemaPermissions {
    let create = authorize(store, claims, ActionVerb::Create, schema, None)
        .map(|d| d.is_allow())
        .unwrap_or(false);
    SchemaPermissions { create }
}

/// Compute per-entity permissions for the caller.
///
/// Both verbs are evaluated against the actual `entity` so attribute-based
/// policies fire correctly — a user who can update entities they own but
/// not those owned by others gets `update: true` only on their own rows.
pub fn entity_permissions(
    store: &Arc<PolicyStore>,
    schema: &SchemaDefinition,
    entity: &Entity,
    claims: Option<&Claims>,
) -> EntityPermissions {
    let update = authorize(store, claims, ActionVerb::Update, schema, Some(entity))
        .map(|d| d.is_allow())
        .unwrap_or(false);
    let delete = authorize(store, claims, ActionVerb::Delete, schema, Some(entity))
        .map(|d| d.is_allow())
        .unwrap_or(false);
    EntityPermissions { update, delete }
}

/// Filter entity fields based on `@field_access` annotations, delegating to
/// the Cedar engine's per-field action evaluation.
///
/// Silently removes fields the user cannot access. Fields without any
/// `@field_access` annotation are always retained (no per-field action is
/// generated for them, so Cedar trivially allows). When `claims` is `None`,
/// no filtering occurs — unauthenticated requests are rejected upstream and
/// any open-access pipeline ought to surface every field.
pub fn filter_entity_fields(
    store: &Arc<PolicyStore>,
    entity: &mut Entity,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    direction: FieldFilterDirection,
) -> Vec<String> {
    if claims.is_none() {
        return Vec::new();
    }

    let cedar_dir = match direction {
        FieldFilterDirection::Read => FieldDirection::Read,
        FieldFilterDirection::Write => FieldDirection::Write,
    };

    let fields_to_remove: Vec<String> = entity
        .fields
        .keys()
        .filter(|field_name| {
            // Only check fields that have a @field_access annotation; any
            // other field is unrestricted by definition.
            let Some(field_def) = schema.field(field_name) else {
                return false;
            };
            if field_def.field_access().is_none() {
                return false;
            }
            match authorize_field(store, claims, schema, entity, field_name, cedar_dir) {
                Ok(d) => !d.is_allow(),
                // Adapter or request errors: deny defensively, surface in audit.
                Err(_) => true,
            }
        })
        .cloned()
        .collect();

    for name in &fields_to_remove {
        entity.fields.remove(name);
    }
    fields_to_remove
}

/// Inject tenant scoping filter into a query.
///
/// Adds `_tenant = <tenant_id>` filter based on the deepest tenant in the
/// claims' `tenant_chain` custom claim. No-ops when:
/// - `tenant_config` is `None` or disabled
/// - `claims` is `None`
/// - user is `platform_admin` (bypass)
pub fn inject_tenant_scope(
    query: &mut Query,
    claims: Option<&Claims>,
    tenant_config: &Option<TenantConfig>,
) {
    let _config = match tenant_config {
        Some(c) if c.is_enabled() => c,
        _ => return,
    };
    let claims = match claims {
        Some(c) => c,
        None => return,
    };
    if claims.has_role(PLATFORM_ADMIN_ROLE) {
        return;
    }
    let tenant_chain: Vec<TenantRef> = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();
    if let Some(tenant_ref) = tenant_chain.last() {
        let tenant_filter = Filter::eq(
            FieldPath::single("_tenant"),
            DynamicValue::Text(tenant_ref.entity_id.clone()),
        );
        query.filter = Some(match query.filter.take() {
            Some(existing) => Filter::and(vec![existing, tenant_filter]),
            None => tenant_filter,
        });
    }
}

/// Inject `_tenant` field into entity fields on creation.
///
/// Sets `_tenant` to the deepest tenant entity ID in the claims'
/// `tenant_chain` custom claim. No-ops when tenancy is disabled,
/// claims is `None`, or the tenant chain is empty.
pub fn inject_tenant_on_create(
    fields: &mut BTreeMap<String, DynamicValue>,
    claims: Option<&Claims>,
    tenant_config: &Option<TenantConfig>,
) {
    let _config = match tenant_config {
        Some(c) if c.is_enabled() => c,
        _ => return,
    };
    let claims = match claims {
        Some(c) => c,
        None => return,
    };
    let tenant_chain: Vec<TenantRef> = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();
    if let Some(tenant_ref) = tenant_chain.last() {
        fields.insert(
            "_tenant".to_string(),
            DynamicValue::Text(tenant_ref.entity_id.clone()),
        );
    }
}

/// Force-set the schema's `@owner` field to the authenticated principal.
///
/// The Cedar `owner_write` / `owner_restrict` policies decide per-record
/// access by comparing `resource.<owner_field> == principal.id`. If the
/// owner field is client-supplied a caller can plant rows under any
/// principal's ID — claiming-by-impersonation, locking themselves out of
/// their own creations, or both. Server-side injection at create time
/// closes this hole.
///
/// Behavior:
/// - No-op when the schema has no `@owner` annotation.
/// - No-op when `claims` is `None` (unauthenticated routes are rejected
///   upstream; for the rare `@access(write: ["public"])` case there is no
///   principal to record).
/// - Overwrites any client-supplied value in the owner field. The client
///   never wins — even when carrying its own principal id, we still
///   re-write it so the server is the single source of truth.
pub fn inject_owner_on_create(
    fields: &mut BTreeMap<String, DynamicValue>,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
) {
    let Some(claims) = claims else { return };
    let Some(owner_field) = schema.fields.iter().find(|f| f.has_owner()) else {
        return;
    };
    // Match the Cedar `Forge::Principal::id` attribute shape — the engine
    // strips the `user:` prefix when building the principal entity (see
    // `crate::authz::adapters::user_id_from_sub`). Storing the prefixed
    // form on the row would silently break every `@owner` policy: the
    // `resource[<owner>] == principal.id` clause would never match.
    let owner_value = user_id_from_sub(&claims.sub).to_string();
    fields.insert(
        owner_field.name.as_str().to_string(),
        DynamicValue::Text(owner_value),
    );
}

/// Drop any client-supplied value for the schema's `@owner` field from a
/// proposed update payload.
///
/// Ownership transfer is a privileged operation; allowing it through a
/// regular PUT/PATCH would let any caller hand their record off to a
/// chosen principal (or worse, masquerade their own writes as a victim's).
/// The Cedar `owner_restrict` policy evaluates against the *current*
/// resource state, so the in-memory check passes — but the persisted
/// owner column would then change. Stripping the field here keeps
/// ownership immutable post-create.
pub fn strip_owner_on_update(
    fields: &mut BTreeMap<String, DynamicValue>,
    schema: &SchemaDefinition,
) {
    if let Some(owner_field) = schema.fields.iter().find(|f| f.has_owner()) {
        fields.remove(owner_field.name.as_str());
    }
}

/// Conventional audit-column field names auto-maintained by the platform.
///
/// `created_by` / `updated_by` carry the principal id, with the `user:`
/// prefix stripped to match Cedar's `principal.id` attribute shape.
/// `created_at` / `updated_at` carry the current wall-clock time.
/// A schema author who declares any subset of these fields gets them
/// populated by the server on create/update — the client never wins.
const AUDIT_COL_CREATED_BY: &str = "created_by";
const AUDIT_COL_UPDATED_BY: &str = "updated_by";
const AUDIT_COL_CREATED_AT: &str = "created_at";
const AUDIT_COL_UPDATED_AT: &str = "updated_at";

/// Auto-populate conventional `created_*` and `updated_*` audit columns
/// on entity creation. See module constants for the supported field names.
///
/// Behavior:
/// - No-op for any column the schema does not declare. Operator opt-in,
///   not enforcement: a schema without these fields gets no audit columns.
/// - Overwrites any client-supplied value. The audit trail is server-owned.
/// - Skips `created_by` / `updated_by` when `claims` is `None`.
pub fn inject_audit_columns_on_create(
    fields: &mut BTreeMap<String, DynamicValue>,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    now: chrono::DateTime<chrono::Utc>,
) {
    // Same stripping as inject_owner_on_create — a schema author who marks
    // `created_by` as `@owner` should get a value that Cedar can match
    // against `principal.id`. See note on inject_owner_on_create.
    let actor = claims.map(|c| user_id_from_sub(&c.sub).to_string());
    inject_audit_actor_field(fields, schema, AUDIT_COL_CREATED_BY, actor.as_deref());
    inject_audit_actor_field(fields, schema, AUDIT_COL_UPDATED_BY, actor.as_deref());
    inject_audit_timestamp_field(fields, schema, AUDIT_COL_CREATED_AT, now);
    inject_audit_timestamp_field(fields, schema, AUDIT_COL_UPDATED_AT, now);
}

/// Auto-populate the conventional `updated_*` audit columns on entity update.
///
/// The create-side columns (`created_by`, `created_at`) are intentionally
/// untouched — they remain at the values stamped during the create that
/// originated the row. Callers must merge this output into the entity's
/// existing field map before persisting.
pub fn inject_audit_columns_on_update(
    fields: &mut BTreeMap<String, DynamicValue>,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    now: chrono::DateTime<chrono::Utc>,
) {
    let actor = claims.map(|c| user_id_from_sub(&c.sub).to_string());
    inject_audit_actor_field(fields, schema, AUDIT_COL_UPDATED_BY, actor.as_deref());
    inject_audit_timestamp_field(fields, schema, AUDIT_COL_UPDATED_AT, now);
}

fn inject_audit_actor_field(
    fields: &mut BTreeMap<String, DynamicValue>,
    schema: &SchemaDefinition,
    name: &str,
    actor: Option<&str>,
) {
    let Some(actor) = actor else { return };
    if schema.fields.iter().any(|f| f.name.as_str() == name) {
        fields.insert(name.to_string(), DynamicValue::Text(actor.to_string()));
    }
}

fn inject_audit_timestamp_field(
    fields: &mut BTreeMap<String, DynamicValue>,
    schema: &SchemaDefinition,
    name: &str,
    at: chrono::DateTime<chrono::Utc>,
) {
    if schema.fields.iter().any(|f| f.name.as_str() == name) {
        fields.insert(name.to_string(), DynamicValue::DateTime(at));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Annotation, EntityId, FieldDefinition, FieldName, FieldType, SchemaId, SchemaName,
        TenantKind, TextConstraints,
    };
    use std::collections::{BTreeMap, HashMap};

    use schema_forge_core::types::DynamicValue;

    fn make_claims(roles: &[&str]) -> Claims {
        Claims {
            sub: format!("user:{}", EntityId::new("user").as_str()),
            roles: roles.iter().map(|r| r.to_string()).collect(),
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

    fn make_claims_with_tenant(roles: &[&str], tenant_entity_id: &str) -> Claims {
        let mut claims = make_claims(roles);
        claims.custom.insert(
            "tenant_chain".to_string(),
            serde_json::json!([{"schema": "Organization", "entity_id": tenant_entity_id}]),
        );
        claims
    }

    fn make_enabled_tenant_config() -> Option<TenantConfig> {
        let schemas = vec![SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Organization").unwrap(),
            vec![make_field("name")],
            vec![Annotation::Tenant(TenantKind::Root)],
        )
        .unwrap()];
        Some(TenantConfig::from_schemas(&schemas).unwrap())
    }

    fn make_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    // -----------------------------------------------------------------------
    // OptionalClaims extractor tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn optional_claims_extracts_when_present() {
        use axum::extract::FromRequestParts;

        let claims = make_claims(&["member"]);
        let (mut parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();
        parts.extensions.insert(claims.clone());

        let result = OptionalClaims::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let OptionalClaims(extracted) = result.unwrap();
        assert!(extracted.is_some());
        assert_eq!(extracted.unwrap().roles, claims.roles);
    }

    #[tokio::test]
    async fn optional_claims_returns_none_when_missing() {
        use axum::extract::FromRequestParts;

        let (mut parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();

        let result = OptionalClaims::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let OptionalClaims(extracted) = result.unwrap();
        assert!(extracted.is_none());
    }

    // -----------------------------------------------------------------------
    // inject_tenant_scope tests
    // -----------------------------------------------------------------------

    #[test]
    fn inject_tenant_scope_adds_filter_when_enabled() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new("tenant");
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_some());
        let filter = query.filter.unwrap();
        match filter {
            Filter::Eq {
                ref path,
                ref value,
            } => {
                assert_eq!(path.root(), "_tenant");
                assert_eq!(*value, DynamicValue::Text(tenant_id.as_str().to_string()));
            }
            _ => panic!("expected Eq filter, got: {filter:?}"),
        }
    }

    #[test]
    fn inject_tenant_scope_noop_when_disabled() {
        let tenant_config: Option<TenantConfig> = None;
        let tenant_id = EntityId::new("tenant");
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_none());
    }

    #[test]
    fn inject_tenant_scope_noop_for_platform_admin() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new("tenant");
        let claims = make_claims_with_tenant(&["platform_admin"], tenant_id.as_str());
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_none());
    }

    #[test]
    fn inject_tenant_scope_noop_when_no_claims() {
        let tenant_config = make_enabled_tenant_config();
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, None, &tenant_config);

        assert!(query.filter.is_none());
    }

    #[test]
    fn inject_tenant_scope_combines_with_existing_filter() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new("tenant");
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());

        let existing_filter = Filter::eq(
            FieldPath::single("status"),
            DynamicValue::Text("active".to_string()),
        );
        let mut query = Query::new(SchemaId::new()).with_filter(existing_filter);

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_some());
        let filter = query.filter.unwrap();
        match filter {
            Filter::And { ref filters } => {
                assert_eq!(filters.len(), 2);
                // First filter is the original
                match &filters[0] {
                    Filter::Eq { path, .. } => assert_eq!(path.root(), "status"),
                    other => panic!("expected Eq filter, got: {other:?}"),
                }
                // Second filter is the tenant filter
                match &filters[1] {
                    Filter::Eq { path, value } => {
                        assert_eq!(path.root(), "_tenant");
                        assert_eq!(*value, DynamicValue::Text(tenant_id.as_str().to_string()));
                    }
                    other => panic!("expected Eq filter, got: {other:?}"),
                }
            }
            _ => panic!("expected And filter, got: {filter:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // inject_tenant_on_create tests
    // -----------------------------------------------------------------------

    #[test]
    fn inject_tenant_on_create_inserts_tenant_field() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new("tenant");
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, Some(&claims), &tenant_config);

        assert!(fields.contains_key("_tenant"));
        assert_eq!(
            fields["_tenant"],
            DynamicValue::Text(tenant_id.as_str().to_string())
        );
    }

    #[test]
    fn inject_tenant_on_create_noop_when_disabled() {
        let tenant_config: Option<TenantConfig> = None;
        let tenant_id = EntityId::new("tenant");
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, Some(&claims), &tenant_config);

        assert!(!fields.contains_key("_tenant"));
    }

    #[test]
    fn inject_tenant_on_create_noop_when_no_claims() {
        let tenant_config = make_enabled_tenant_config();
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, None, &tenant_config);

        assert!(!fields.contains_key("_tenant"));
    }

    #[test]
    fn inject_tenant_on_create_noop_when_empty_tenant_chain() {
        let tenant_config = make_enabled_tenant_config();
        let claims = make_claims(&["member"]); // no tenant chain
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, Some(&claims), &tenant_config);

        assert!(!fields.contains_key("_tenant"));
    }

    // -----------------------------------------------------------------------
    // inject_owner_on_create / strip_owner_on_update tests
    // -----------------------------------------------------------------------

    fn schema_with_owner(name: &str, owner_field: &str) -> SchemaDefinition {
        use schema_forge_core::types::FieldAnnotation;
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("title").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new(owner_field).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![],
                    vec![FieldAnnotation::Owner],
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    fn schema_without_owner(name: &str) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn inject_owner_on_create_sets_owner_to_principal_sub() {
        let schema = schema_with_owner("Note", "created_by");
        let claims = make_claims(&["member"]);
        let mut fields = BTreeMap::new();
        fields.insert("title".to_string(), DynamicValue::Text("hello".to_string()));

        inject_owner_on_create(&mut fields, &schema, Some(&claims));

        assert_eq!(
            fields["created_by"],
            DynamicValue::Text(user_id_from_sub(&claims.sub).to_string())
        );
    }

    #[test]
    fn inject_owner_on_create_overwrites_client_supplied_value() {
        // Regression: a malicious client supplies a different principal in the
        // owner field to claim a record under someone else's id. The server
        // must overwrite it unconditionally.
        let schema = schema_with_owner("Note", "created_by");
        let claims = make_claims(&["member"]);
        let mut fields = BTreeMap::new();
        fields.insert(
            "created_by".to_string(),
            DynamicValue::Text("user:victim".to_string()),
        );

        inject_owner_on_create(&mut fields, &schema, Some(&claims));

        assert_eq!(
            fields["created_by"],
            DynamicValue::Text(user_id_from_sub(&claims.sub).to_string())
        );
    }

    #[test]
    fn inject_owner_on_create_noop_when_schema_has_no_owner() {
        let schema = schema_without_owner("Plain");
        let claims = make_claims(&["member"]);
        let mut fields = BTreeMap::new();
        fields.insert("title".to_string(), DynamicValue::Text("x".to_string()));

        inject_owner_on_create(&mut fields, &schema, Some(&claims));

        assert!(!fields.contains_key("created_by"));
    }

    #[test]
    fn inject_owner_on_create_noop_when_unauthenticated() {
        let schema = schema_with_owner("Note", "created_by");
        let mut fields = BTreeMap::new();

        inject_owner_on_create(&mut fields, &schema, None);

        assert!(!fields.contains_key("created_by"));
    }

    #[test]
    fn strip_owner_on_update_drops_owner_field_if_present() {
        // Regression: ownership transfer through PUT/PATCH is forbidden.
        let schema = schema_with_owner("Note", "created_by");
        let mut fields = BTreeMap::new();
        fields.insert("title".to_string(), DynamicValue::Text("new".to_string()));
        fields.insert(
            "created_by".to_string(),
            DynamicValue::Text("user:attacker".to_string()),
        );

        strip_owner_on_update(&mut fields, &schema);

        assert!(!fields.contains_key("created_by"));
        assert!(fields.contains_key("title"));
    }

    #[test]
    fn strip_owner_on_update_noop_when_schema_has_no_owner() {
        let schema = schema_without_owner("Plain");
        let mut fields = BTreeMap::new();
        fields.insert(
            "created_by".to_string(),
            DynamicValue::Text("anything".to_string()),
        );

        strip_owner_on_update(&mut fields, &schema);

        assert!(fields.contains_key("created_by"));
    }

    // -----------------------------------------------------------------------
    // inject_audit_columns tests
    // -----------------------------------------------------------------------

    fn schema_with_audit_columns() -> SchemaDefinition {
        use schema_forge_core::types::FieldType as FT;
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Audited").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("title").unwrap(),
                    FT::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::new(FieldName::new("created_by").unwrap(), FT::Text(TextConstraints::unconstrained())),
                FieldDefinition::new(FieldName::new("updated_by").unwrap(), FT::Text(TextConstraints::unconstrained())),
                FieldDefinition::new(FieldName::new("created_at").unwrap(), FT::DateTime),
                FieldDefinition::new(FieldName::new("updated_at").unwrap(), FT::DateTime),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn inject_audit_columns_on_create_fills_all_four_when_declared() {
        let schema = schema_with_audit_columns();
        let claims = make_claims(&["member"]);
        let now = chrono::Utc::now();
        let mut fields = BTreeMap::new();

        inject_audit_columns_on_create(&mut fields, &schema, Some(&claims), now);

        let expected_actor = user_id_from_sub(&claims.sub).to_string();
        assert_eq!(fields["created_by"], DynamicValue::Text(expected_actor.clone()));
        assert_eq!(fields["updated_by"], DynamicValue::Text(expected_actor));
        assert_eq!(fields["created_at"], DynamicValue::DateTime(now));
        assert_eq!(fields["updated_at"], DynamicValue::DateTime(now));
    }

    #[test]
    fn inject_audit_columns_on_create_overwrites_client_supplied_actor() {
        // Client tries to attribute the create to another user; server wins.
        let schema = schema_with_audit_columns();
        let claims = make_claims(&["member"]);
        let now = chrono::Utc::now();
        let mut fields = BTreeMap::new();
        fields.insert(
            "created_by".to_string(),
            DynamicValue::Text("user:spoofed".to_string()),
        );

        inject_audit_columns_on_create(&mut fields, &schema, Some(&claims), now);

        assert_eq!(
            fields["created_by"],
            DynamicValue::Text(user_id_from_sub(&claims.sub).to_string())
        );
    }

    #[test]
    fn inject_audit_columns_on_create_skips_fields_not_in_schema() {
        let schema = schema_without_owner("Plain");
        let claims = make_claims(&["member"]);
        let now = chrono::Utc::now();
        let mut fields = BTreeMap::new();

        inject_audit_columns_on_create(&mut fields, &schema, Some(&claims), now);

        assert!(!fields.contains_key("created_by"));
        assert!(!fields.contains_key("created_at"));
        assert!(!fields.contains_key("updated_by"));
        assert!(!fields.contains_key("updated_at"));
    }

    #[test]
    fn inject_audit_columns_on_update_touches_only_updated_columns() {
        let schema = schema_with_audit_columns();
        let claims = make_claims(&["member"]);
        let now = chrono::Utc::now();
        let mut fields = BTreeMap::new();
        // Simulate prior values that an update payload might carry — the
        // create-side columns must NOT be overwritten by update injection.
        let earlier = now - chrono::Duration::hours(1);
        fields.insert("created_at".to_string(), DynamicValue::DateTime(earlier));
        fields.insert(
            "created_by".to_string(),
            DynamicValue::Text("user:original".to_string()),
        );

        inject_audit_columns_on_update(&mut fields, &schema, Some(&claims), now);

        // updated_* set, created_* untouched
        assert_eq!(
            fields["updated_by"],
            DynamicValue::Text(user_id_from_sub(&claims.sub).to_string())
        );
        assert_eq!(fields["updated_at"], DynamicValue::DateTime(now));
        assert_eq!(
            fields["created_by"],
            DynamicValue::Text("user:original".to_string())
        );
        assert_eq!(fields["created_at"], DynamicValue::DateTime(earlier));
    }
}
