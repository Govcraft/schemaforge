//! `tenant_scope` middleware — per-request active-tenant validation and
//! hierarchy walk.
//!
//! The login handler ships a token whose `tenant_chain` custom claim is
//! the *flat* set of tenants the user belongs to. This middleware sits
//! between the acton-service token middleware and the SchemaForge route
//! handlers and rewrites that claim into the *effective* per-request
//! scope:
//!
//! 1. Reads `X-Active-Tenant: <schema>:<entity_id>` from the request
//!    (or defaults to the user's single membership if exactly one).
//! 2. Validates the active tenant is in the user's memberships (rejects
//!    impersonation: a token holder cannot claim a tenant they aren't a
//!    member of).
//! 3. Walks the [`TenantConfig`] hierarchy from the active leaf up to
//!    root, fetching each level's entity row to read its `parent_field`
//!    ref. The resulting walk is what downstream code (`access.rs`,
//!    `authz/adapters.rs`) reads from the claim.
//!
//! Bypass paths:
//! - `TenantConfig` is `None` or disabled → passthrough.
//! - No `Claims` in request extensions → passthrough (public routes
//!   served before token middleware sees them; protected routes that
//!   reach here without claims will 401 downstream regardless).
//! - `platform_admin` role → passthrough. Matches the existing
//!   `access.rs::inject_tenant_scope` bypass at line 297.
//! - Empty `tenant_chain` claim (e.g. a `platform_admin` whose login
//!   produced no memberships) → passthrough with the claim untouched.

use std::sync::Arc;

use acton_service::middleware::Claims;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use schema_forge_backend::tenant::{TenantConfig, TenantRef};
use schema_forge_backend::DynEntityStore;
use schema_forge_core::types::{DynamicValue, SchemaName};

use crate::access::PLATFORM_ADMIN_ROLE;

/// State injected into the [`middleware`] function via
/// [`axum::middleware::from_fn_with_state`].
#[derive(Clone)]
pub struct TenantScopeState {
    /// Backend handle for fetching tenant entities during the hierarchy walk.
    pub entity_store: Arc<dyn DynEntityStore>,
    /// Tenant configuration derived from `@tenant` annotations on
    /// registered schemas. `None` (or `Some(cfg)` with
    /// `cfg.is_enabled() == false`) disables this middleware entirely.
    pub tenant_config: Arc<Option<TenantConfig>>,
}

/// `X-Active-Tenant: <schema>:<entity_id>` header name.
pub const ACTIVE_TENANT_HEADER: &str = "x-active-tenant";

/// Error envelope returned for tenant-scope refusals.
///
/// Matches the shape used by `routes::auth::LoginErrorBody` so clients
/// can parse error codes uniformly across auth- and authz-style refusals.
#[derive(serde::Serialize)]
struct TenantScopeError {
    error: &'static str,
    code: &'static str,
    status: u16,
}

/// The middleware function. Wire with
/// `axum::middleware::from_fn_with_state(state, tenant_scope::middleware)`
/// AFTER the token middleware (so Claims exist in extensions) and BEFORE
/// the forge route handlers.
pub async fn middleware<B>(
    State(state): State<TenantScopeState>,
    mut request: Request<B>,
    next: Next,
) -> Response
where
    B: Send + 'static,
    Request<B>: Into<Request<axum::body::Body>>,
{
    // Tenancy disabled at the deployment level — nothing to do.
    let Some(tenant_config) = state.tenant_config.as_ref() else {
        return next.run(request.into()).await;
    };
    if !tenant_config.is_enabled() {
        return next.run(request.into()).await;
    }

    let Some(claims) = request.extensions().get::<Claims>().cloned() else {
        // Public route or unauthenticated request — let the downstream
        // 401 path (or public-route allow) handle it.
        return next.run(request.into()).await;
    };

    if claims.has_role(PLATFORM_ADMIN_ROLE) {
        // Platform admin bypasses tenancy. Pass claims through unmodified.
        return next.run(request.into()).await;
    }

    let memberships: Vec<TenantRef> = claims
        .custom_claim_as("tenant_chain")
        .unwrap_or_default();

    // Empty memberships means the login handler refused or the user is a
    // platform_admin caught upstream; downstream `_tenant in principal`
    // will simply not match anything. Pass through.
    if memberships.is_empty() {
        return next.run(request.into()).await;
    }

    let active = match select_active_tenant(&request, &memberships) {
        Ok(t) => t,
        Err(refusal) => return refusal.into_response(),
    };

    let effective = match walk_to_root(active, tenant_config, state.entity_store.as_ref()).await {
        Ok(walk) => walk,
        Err(WalkError::EntityMissing { schema, entity_id }) => {
            tracing::warn!(
                schema = %schema,
                entity_id = %entity_id,
                "tenant_scope: leaf entity missing during hierarchy walk; \
                 effective chain reduced to leaf-only"
            );
            // Falling back to leaf-only keeps the request servable; Cedar
            // `_tenant in principal` will still match the leaf even if
            // ancestors couldn't be resolved.
            vec![active.clone()]
        }
        Err(WalkError::Backend(e)) => {
            tracing::error!(error = %e, "tenant_scope: backend error during hierarchy walk");
            return internal_error("backend error during tenant hierarchy walk");
        }
    };

    let mut new_claims = claims;
    match serde_json::to_value(&effective) {
        Ok(v) => {
            new_claims.custom.insert("tenant_chain".to_string(), v);
        }
        Err(e) => {
            tracing::error!(error = %e, "tenant_scope: failed to serialize effective chain");
            return internal_error("failed to serialize effective tenant chain");
        }
    }
    request.extensions_mut().insert(new_claims);

    next.run(request.into()).await
}

/// Per-request refusal that must short-circuit before the inner handler
/// runs. Returned by [`select_active_tenant`].
#[derive(Debug)]
enum TenantScopeRefusal {
    /// User has multiple memberships and did not supply `X-Active-Tenant`.
    MissingActiveTenant,
    /// `X-Active-Tenant` value is malformed (not `<schema>:<entity_id>`).
    InvalidActiveTenant,
    /// `X-Active-Tenant` references a tenant the user is not a member of.
    NotInMemberships,
}

impl IntoResponse for TenantScopeRefusal {
    fn into_response(self) -> Response {
        match self {
            Self::MissingActiveTenant => (
                StatusCode::BAD_REQUEST,
                Json(TenantScopeError {
                    error: "X-Active-Tenant required: user has multiple tenant memberships",
                    code: "ACTIVE_TENANT_REQUIRED",
                    status: 400,
                }),
            )
                .into_response(),
            Self::InvalidActiveTenant => (
                StatusCode::BAD_REQUEST,
                Json(TenantScopeError {
                    error: "X-Active-Tenant must be of the form <schema>:<entity_id>",
                    code: "ACTIVE_TENANT_INVALID",
                    status: 400,
                }),
            )
                .into_response(),
            Self::NotInMemberships => (
                StatusCode::FORBIDDEN,
                Json(TenantScopeError {
                    error: "X-Active-Tenant not in user memberships",
                    code: "ACTIVE_TENANT_FORBIDDEN",
                    status: 403,
                }),
            )
                .into_response(),
        }
    }
}

fn internal_error(message: &'static str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": "internal_error",
            "message": message,
        })),
    )
        .into_response()
}

/// Decide which membership scopes this request. Pure function over the
/// request's `X-Active-Tenant` header and the user's membership set.
fn select_active_tenant<'a, B>(
    request: &Request<B>,
    memberships: &'a [TenantRef],
) -> Result<&'a TenantRef, TenantScopeRefusal> {
    debug_assert!(
        !memberships.is_empty(),
        "select_active_tenant called with empty memberships"
    );
    let header = request
        .headers()
        .get(ACTIVE_TENANT_HEADER)
        .and_then(|v| v.to_str().ok());

    let Some(header_value) = header else {
        if memberships.len() == 1 {
            return Ok(&memberships[0]);
        }
        return Err(TenantScopeRefusal::MissingActiveTenant);
    };

    let parsed = parse_active_tenant(header_value)
        .ok_or(TenantScopeRefusal::InvalidActiveTenant)?;
    memberships
        .iter()
        .find(|m| m.schema == parsed.0 && m.entity_id == parsed.1)
        .ok_or(TenantScopeRefusal::NotInMemberships)
}

/// Parse a `<schema>:<entity_id>` header value.
///
/// Returns `None` for any malformed input. The entity_id half may itself
/// contain colons (TypeIDs use `_`, not `:`, but we don't enforce that
/// here — only the first `:` is treated as the separator).
fn parse_active_tenant(s: &str) -> Option<(String, String)> {
    let (schema, rest) = s.split_once(':')?;
    if schema.is_empty() || rest.is_empty() {
        return None;
    }
    Some((schema.to_string(), rest.to_string()))
}

/// Errors raised during the hierarchy walk.
#[derive(Debug)]
enum WalkError {
    /// A leaf or intermediate entity couldn't be found. Treated as
    /// non-fatal by the caller — the effective chain collapses to just
    /// the active leaf.
    EntityMissing {
        schema: String,
        entity_id: String,
    },
    /// Backend errored on a `get`. Treated as fatal (500).
    Backend(schema_forge_backend::error::BackendError),
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EntityMissing { schema, entity_id } => {
                write!(f, "entity {schema}/{entity_id} missing during walk")
            }
            Self::Backend(e) => write!(f, "{e}"),
        }
    }
}

/// Walk the tenant hierarchy from `leaf` up to the root, returning the
/// effective chain in root→leaf order.
///
/// At each step, looks up the level's `parent_field` from `TenantConfig`
/// then fetches the current entity to read the referenced parent id. If
/// the active leaf's schema isn't in the hierarchy at all (stale
/// membership row pointing to a non-tenant schema), returns just the
/// leaf — the request stays servable but won't unlock anything above
/// the leaf in Cedar's parent set.
async fn walk_to_root(
    leaf: &TenantRef,
    tenant_config: &TenantConfig,
    store: &dyn DynEntityStore,
) -> Result<Vec<TenantRef>, WalkError> {
    let mut chain: Vec<TenantRef> = vec![leaf.clone()];

    let mut current_schema = leaf.schema.clone();
    let mut current_id = leaf.entity_id.clone();

    loop {
        // Look up the current level in TenantConfig. If the active leaf's
        // schema isn't a registered tenant level, there's nothing to walk
        // — return what we have.
        let Some(level) = tenant_config
            .hierarchy
            .iter()
            .find(|l| l.schema.as_str() == current_schema.as_str())
        else {
            break;
        };

        // Reached the root: no parent to walk to.
        let (Some(parent_schema), Some(parent_field)) = (&level.parent, &level.parent_field) else {
            break;
        };

        // Fetch the current entity to read its parent reference field.
        let schema_name = SchemaName::new(&current_schema).map_err(|_| WalkError::EntityMissing {
            schema: current_schema.clone(),
            entity_id: current_id.clone(),
        })?;
        let entity_id =
            schema_forge_core::types::EntityId::parse(&current_id).map_err(|_| WalkError::EntityMissing {
                schema: current_schema.clone(),
                entity_id: current_id.clone(),
            })?;

        let entity = match store.get(&schema_name, &entity_id).await {
            Ok(e) => e,
            Err(schema_forge_backend::error::BackendError::EntityNotFound { .. }) => {
                return Err(WalkError::EntityMissing {
                    schema: current_schema,
                    entity_id: current_id,
                });
            }
            Err(e) => return Err(WalkError::Backend(e)),
        };

        let parent_value = entity.field(parent_field.as_str());
        let parent_id = match parent_value {
            Some(DynamicValue::Ref(id)) => id.as_str().to_string(),
            // Composite or missing — can't walk further. The chain is
            // capped at what we've collected so far.
            _ => break,
        };

        let parent_ref = TenantRef {
            schema: parent_schema.as_str().to_string(),
            entity_id: parent_id.clone(),
        };
        chain.push(parent_ref);

        current_schema = parent_schema.as_str().to_string();
        current_id = parent_id;
    }

    // Caller wants root→leaf order; reverse so the deepest tenant lands at
    // `chain.last()` (matches the historic naming the old code used).
    chain.reverse();
    Ok(chain)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    fn membership(schema: &str, id: &str) -> TenantRef {
        TenantRef {
            schema: schema.to_string(),
            entity_id: id.to_string(),
        }
    }

    fn req(headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().uri("/api/v1/forge/schemas/Foo/entities");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn parse_active_tenant_accepts_well_formed_pair() {
        let (s, id) = parse_active_tenant("Organization:org-a").unwrap();
        assert_eq!(s, "Organization");
        assert_eq!(id, "org-a");
    }

    #[test]
    fn parse_active_tenant_rejects_missing_separator() {
        assert!(parse_active_tenant("Organization-org-a").is_none());
    }

    #[test]
    fn parse_active_tenant_rejects_empty_halves() {
        assert!(parse_active_tenant(":org-a").is_none());
        assert!(parse_active_tenant("Organization:").is_none());
        assert!(parse_active_tenant(":").is_none());
    }

    #[test]
    fn select_active_tenant_uses_sole_membership_when_header_absent() {
        let memberships = vec![membership("Organization", "org-a")];
        let request = req(&[]);
        let active = select_active_tenant(&request, &memberships).unwrap();
        assert_eq!(active.entity_id, "org-a");
    }

    #[test]
    fn select_active_tenant_refuses_when_header_missing_and_multi_membership() {
        let memberships = vec![
            membership("Organization", "org-a"),
            membership("Organization", "org-b"),
        ];
        let request = req(&[]);
        let err = select_active_tenant(&request, &memberships).unwrap_err();
        assert!(matches!(err, TenantScopeRefusal::MissingActiveTenant));
    }

    #[test]
    fn select_active_tenant_uses_header_when_present_and_valid() {
        let memberships = vec![
            membership("Organization", "org-a"),
            membership("Organization", "org-b"),
        ];
        let request = req(&[("x-active-tenant", "Organization:org-b")]);
        let active = select_active_tenant(&request, &memberships).unwrap();
        assert_eq!(active.entity_id, "org-b");
    }

    #[test]
    fn select_active_tenant_refuses_header_not_in_memberships() {
        let memberships = vec![membership("Organization", "org-a")];
        let request = req(&[("x-active-tenant", "Organization:org-b")]);
        let err = select_active_tenant(&request, &memberships).unwrap_err();
        assert!(matches!(err, TenantScopeRefusal::NotInMemberships));
    }

    #[test]
    fn select_active_tenant_refuses_malformed_header() {
        let memberships = vec![
            membership("Organization", "org-a"),
            membership("Organization", "org-b"),
        ];
        let request = req(&[("x-active-tenant", "garbage-no-colon")]);
        let err = select_active_tenant(&request, &memberships).unwrap_err();
        assert!(matches!(err, TenantScopeRefusal::InvalidActiveTenant));
    }

    /// Build a `TenantConfig` for [Organization (root), Department (child of Organization)]
    /// and a `MemStore` containing a single Department row whose `organization`
    /// ref points at "Organization::org-a".
    async fn config_with_org_dept_hierarchy() -> (TenantConfig, Arc<crate::middleware::tenant_scope::test_support::MemStore>) {
        use crate::middleware::tenant_scope::test_support::MemStore;
        use schema_forge_backend::Entity;
        use schema_forge_core::types::{
            Annotation, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaId,
            SchemaName as CoreSchemaName, TenantKind, TextConstraints,
        };
        use schema_forge_core::types::SchemaDefinition;

        let org_schema = SchemaDefinition::new(
            SchemaId::new(),
            CoreSchemaName::new("Organization").unwrap(),
            vec![FieldDefinition::with_annotations(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
                vec![],
            )],
            vec![Annotation::Tenant(TenantKind::Root)],
        )
        .unwrap();

        let dept_schema = SchemaDefinition::new(
            SchemaId::new(),
            CoreSchemaName::new("Department").unwrap(),
            vec![
                FieldDefinition::with_annotations(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("organization").unwrap(),
                    FieldType::Relation {
                        target: CoreSchemaName::new("Organization").unwrap(),
                        cardinality: schema_forge_core::types::Cardinality::One,
                    },
                    vec![FieldModifier::Required],
                    vec![],
                ),
            ],
            vec![Annotation::Tenant(TenantKind::Child {
                parent: CoreSchemaName::new("Organization").unwrap(),
            })],
        )
        .unwrap();

        let cfg = TenantConfig::from_schemas(&[org_schema.clone(), dept_schema.clone()]).unwrap();
        assert!(cfg.is_enabled());
        assert_eq!(cfg.hierarchy.len(), 2);

        let store = Arc::new(MemStore::new());

        // Seed a Department row with `organization` -> Ref(<org-a entity id>).
        // The walk parses Department's `current_id` via EntityId::parse, so we
        // mint a real EntityId for both the department and the organization.
        let org_id = schema_forge_core::types::EntityId::new("Organization");
        let dept_id = schema_forge_core::types::EntityId::new("Department");

        use schema_forge_backend::traits::EntityStore;
        let mut org_fields: std::collections::BTreeMap<String, DynamicValue> =
            std::collections::BTreeMap::new();
        org_fields.insert("name".into(), DynamicValue::Text("Org A".into()));
        EntityStore::create(
            store.as_ref(),
            &Entity::with_id(org_id.clone(), org_schema.name.clone(), org_fields),
        )
        .await
        .unwrap();

        let mut dept_fields: std::collections::BTreeMap<String, DynamicValue> =
            std::collections::BTreeMap::new();
        dept_fields.insert("name".into(), DynamicValue::Text("Dept 1".into()));
        dept_fields.insert("organization".into(), DynamicValue::Ref(org_id.clone()));
        EntityStore::create(
            store.as_ref(),
            &Entity::with_id(dept_id.clone(), dept_schema.name.clone(), dept_fields),
        )
        .await
        .unwrap();

        // Stash the ids in the store so the test can read them back.
        store.set_seed_ids(org_id.as_str(), dept_id.as_str());

        (cfg, store)
    }

    #[tokio::test]
    async fn walk_to_root_returns_leaf_when_schema_not_in_hierarchy() {
        use crate::middleware::tenant_scope::test_support::MemStore;
        let cfg = TenantConfig::from_schemas(&[]).unwrap();
        let store = Arc::new(MemStore::new());
        let leaf = membership("Random", "rnd-1");
        let walk = walk_to_root(&leaf, &cfg, store.as_ref()).await.unwrap();
        assert_eq!(walk, vec![leaf]);
    }

    #[tokio::test]
    async fn walk_to_root_walks_two_level_hierarchy() {
        let (cfg, store) = config_with_org_dept_hierarchy().await;
        let (org_id, dept_id) = store.seed_ids();
        let leaf = membership("Department", &dept_id);
        let walk = walk_to_root(&leaf, &cfg, store.as_ref()).await.unwrap();
        // root→leaf order: Organization first, Department last.
        assert_eq!(walk.len(), 2);
        assert_eq!(walk[0].schema, "Organization");
        assert_eq!(walk[0].entity_id, org_id);
        assert_eq!(walk[1].schema, "Department");
        assert_eq!(walk[1].entity_id, dept_id);
    }

    #[tokio::test]
    async fn walk_to_root_at_root_returns_just_root() {
        let (cfg, store) = config_with_org_dept_hierarchy().await;
        let (org_id, _) = store.seed_ids();
        let leaf = membership("Organization", &org_id);
        let walk = walk_to_root(&leaf, &cfg, store.as_ref()).await.unwrap();
        assert_eq!(walk.len(), 1);
        assert_eq!(walk[0].entity_id, org_id);
    }
}

#[cfg(test)]
mod test_support {
    //! Tiny in-memory `EntityStore`/`DynEntityStore` for the tenant-scope
    //! middleware tests. Kept separate from the test module so the hierarchy
    //! walk's helpers can name the type by path.
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use schema_forge_backend::entity::{Entity, QueryResult};
    use schema_forge_backend::error::BackendError;
    use schema_forge_backend::traits::EntityStore;
    use schema_forge_core::query::Query;
    use schema_forge_core::types::{EntityId, SchemaName};

    #[derive(Default)]
    pub struct MemStore {
        rows: Mutex<Vec<Entity>>,
        seed_ids: Mutex<(String, String)>,
    }

    impl MemStore {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn set_seed_ids(&self, org: &str, dept: &str) {
            *self.seed_ids.lock().unwrap() = (org.to_string(), dept.to_string());
        }

        pub fn seed_ids(&self) -> (String, String) {
            self.seed_ids.lock().unwrap().clone()
        }
    }

    impl EntityStore for MemStore {
        fn create(
            &self,
            entity: &Entity,
        ) -> impl std::future::Future<Output = Result<Entity, BackendError>> + Send {
            let entity = entity.clone();
            async move {
                self.rows.lock().unwrap().push(entity.clone());
                Ok(entity)
            }
        }

        fn get(
            &self,
            schema: &SchemaName,
            id: &EntityId,
        ) -> impl std::future::Future<Output = Result<Entity, BackendError>> + Send {
            let schema = schema.as_str().to_string();
            let id = id.clone();
            async move {
                self.rows
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|e| e.id == id)
                    .cloned()
                    .ok_or(BackendError::EntityNotFound {
                        schema,
                        entity_id: id.as_str().to_string(),
                    })
            }
        }

        fn update(
            &self,
            entity: &Entity,
        ) -> impl std::future::Future<Output = Result<Entity, BackendError>> + Send {
            let entity = entity.clone();
            async move {
                let mut rows = self.rows.lock().unwrap();
                if let Some(slot) = rows.iter_mut().find(|e| e.id == entity.id) {
                    *slot = entity.clone();
                    Ok(entity)
                } else {
                    Err(BackendError::EntityNotFound {
                        schema: entity.schema.as_str().to_string(),
                        entity_id: entity.id.as_str().to_string(),
                    })
                }
            }
        }

        fn delete(
            &self,
            _schema: &SchemaName,
            id: &EntityId,
        ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send {
            let id = id.clone();
            async move {
                self.rows.lock().unwrap().retain(|e| e.id != id);
                Ok(())
            }
        }

        async fn query(&self, _query: &Query) -> Result<QueryResult, BackendError> {
            Ok(QueryResult::new(Vec::new(), Some(0)))
        }

        async fn count(&self, _query: &Query) -> Result<usize, BackendError> {
            Ok(0)
        }

        async fn aggregate(
            &self,
            _query: &schema_forge_core::query::AggregateQuery,
        ) -> Result<Vec<schema_forge_core::query::AggregateResult>, BackendError> {
            Ok(Vec::new())
        }
    }

    // Helper to let the middleware test module pass `Arc<MemStore>` where an
    // `&dyn DynEntityStore` is expected: the blanket impl of DynEntityStore
    // for any EntityStore does the heavy lifting; we just need the concrete
    // type to be reachable from the test module by path.
    static _ASSERT_MEMSTORE_USABLE: fn() = || {
        let _: &dyn schema_forge_backend::DynEntityStore = &MemStore::new();
    };

    // Used by the unused-import lint to avoid removing the BTreeMap import
    // for the public `Entity::new` arms our test paths exercise.
    fn _force_btreemap_use(_: BTreeMap<String, String>) {}
}
