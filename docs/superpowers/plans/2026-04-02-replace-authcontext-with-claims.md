# Replace AuthContext with acton-service Claims — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace SchemaForge's custom `AuthContext`/`AuthProvider` auth stack with acton-service's built-in `Claims` type and `PasetoAuth` middleware.

**Architecture:** Remove the parallel auth abstraction. `schema-forge-backend` gains acton-service as a dependency for the `Claims` type. All access control functions switch from `AuthContext` to `Claims`. Token validation is delegated entirely to acton-service's `PasetoAuth` middleware. `TenantRef` becomes a serializable struct for PASETO custom claim round-tripping.

**Tech Stack:** acton-service v0.21 (Claims, PasetoAuth), PASETO V4 tokens, serde for custom claim serialization

**Spec:** `docs/superpowers/specs/2026-04-02-replace-authcontext-with-claims-design.md`

---

## File Map

### Files to delete
- `crates/schema-forge-acton/src/auth.rs` — `AuthProvider` trait, `NoopAuthProvider`
- `crates/schema-forge-acton/src/middleware.rs` — `auth_middleware`

### Files to modify
- `crates/schema-forge-backend/Cargo.toml` — add acton-service dependency
- `crates/schema-forge-backend/src/auth.rs` — remove `AuthContext`/`AuthError`/`TenantRef`, update `RecordAccessPolicy` to use `Claims`
- `crates/schema-forge-backend/src/tenant.rs` — add `TenantRef` (serializable, for custom claims)
- `crates/schema-forge-backend/src/lib.rs` — update re-exports
- `crates/schema-forge-acton/src/lib.rs` — remove `pub mod auth` and `pub mod middleware`
- `crates/schema-forge-acton/src/access.rs` — replace `OptionalAuth` with `OptionalClaims`, update all functions
- `crates/schema-forge-acton/src/state.rs` — remove `auth_provider` from `ForgeState`
- `crates/schema-forge-acton/src/extension.rs` — remove `with_auth_provider()`, remove auth middleware layers
- `crates/schema-forge-acton/src/routes/entities.rs` — `OptionalClaims` in all handlers
- `crates/schema-forge-acton/src/graphql/context.rs` — `Claims` instead of `AuthContext`
- `crates/schema-forge-acton/src/graphql/mod.rs` — `OptionalClaims` in graphql handler
- `crates/schema-forge-acton/src/graphql/resolvers.rs` — `Claims` in resolver functions
- `crates/schema-forge-acton/tests/integration.rs` — remove auth_provider, update test helpers
- `crates/schema-forge-acton/tests/auth_demo.rs` — replace ConfigurableAuthProvider with Claims injection
- `crates/schema-forge-acton/tests/admin_integration.rs` — remove auth_provider from ForgeState

---

## Task 1: Add acton-service dependency to schema-forge-backend and move TenantRef

**Files:**
- Modify: `crates/schema-forge-backend/Cargo.toml`
- Modify: `crates/schema-forge-backend/src/tenant.rs`
- Modify: `crates/schema-forge-backend/src/lib.rs`

- [ ] **Step 1: Add acton-service dependency**

```bash
cd /home/rodzilla/code/active/schemaforge/crates/schema-forge-backend && cargo add acton-service --no-default-features
```

- [ ] **Step 2: Add serializable TenantRef to tenant.rs**

Add at the end of `crates/schema-forge-backend/src/tenant.rs` (before any existing tests):

```rust
/// A reference to a tenant entity, serializable for PASETO custom claims.
///
/// Used in the `tenant_chain` custom claim to identify which tenant(s)
/// scope the authenticated user's access.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TenantRef {
    /// The schema name of the tenant type (e.g., "Organization").
    pub schema: String,
    /// The entity ID of the specific tenant.
    pub entity_id: String,
}
```

- [ ] **Step 3: Update lib.rs re-exports**

In `crates/schema-forge-backend/src/lib.rs`, change:

```rust
pub use auth::{AuthContext, AuthError, OwnershipBasedPolicy, RecordAccessPolicy, TenantRef};
```

to:

```rust
pub use auth::{OwnershipBasedPolicy, RecordAccessPolicy};
pub use tenant::TenantRef;
```

- [ ] **Step 4: Verify it compiles**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check -p schema-forge-backend
```

Expected: compilation errors in auth.rs about `AuthContext` still referencing old `TenantRef` — that's fine, we fix it in Task 2.

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-backend/
git commit -S -m "refactor(backend): add acton-service dep, move TenantRef to tenant module"
```

---

## Task 2: Rewrite schema-forge-backend/src/auth.rs to use Claims

**Files:**
- Modify: `crates/schema-forge-backend/src/auth.rs`

- [ ] **Step 1: Write failing tests for Claims-based RecordAccessPolicy**

Replace the existing test helpers and add new Claims-based tests at the bottom of `crates/schema-forge-backend/src/auth.rs`. The test module should construct `Claims` instead of `AuthContext`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use acton_service::middleware::Claims;
    use std::collections::HashMap;

    fn make_claims(roles: &[&str]) -> Claims {
        Claims {
            sub: format!("user:{}", EntityId::new().as_str()),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            perms: vec![],
            exp: 9999999999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            email: None,
            username: None,
            custom: HashMap::new(),
        }
    }

    fn make_claims_with_sub(sub: &str, roles: &[&str]) -> Claims {
        Claims {
            sub: sub.to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            perms: vec![],
            exp: 9999999999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            email: None,
            username: None,
            custom: HashMap::new(),
        }
    }

    #[test]
    fn claims_has_role_returns_true_for_matching_role() {
        let claims = make_claims(&["admin", "member"]);
        assert!(claims.has_role("admin"));
    }

    #[test]
    fn claims_has_role_returns_false_for_missing_role() {
        let claims = make_claims(&["member"]);
        assert!(!claims.has_role("admin"));
    }

    #[test]
    fn claims_is_admin_via_has_role() {
        let claims = make_claims(&["admin"]);
        assert!(claims.has_role("admin"));
    }

    // ... keep all existing OwnershipBasedPolicy tests, updated to use Claims
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo nextest run -p schema-forge-backend
```

Expected: FAIL — `RecordAccessPolicy` still takes `AuthContext`.

- [ ] **Step 3: Rewrite auth.rs implementation**

Replace the entire `crates/schema-forge-backend/src/auth.rs` with the Claims-based version. Key changes:

Remove `AuthContext`, `AuthError`, `TenantRef` (TenantRef moved to tenant.rs in Task 1).

Update `RecordAccessPolicy` trait:

```rust
use acton_service::middleware::Claims;
use schema_forge_core::types::{DynamicValue, EntityId, FieldAnnotation, SchemaDefinition};
use crate::entity::Entity;
use std::future::Future;
use std::pin::Pin;

/// Trait for record-level access control.
pub trait RecordAccessPolicy: Send + Sync {
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>>;

    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}
```

Update `OwnershipBasedPolicy` to use `claims.has_role("admin")` instead of `auth.is_admin()`, and `claims.sub.as_str()` for user ID comparison (strip `user:` prefix if present via a helper):

```rust
/// Extract the user entity ID from a Claims subject.
///
/// Supports both plain IDs ("entity_abc123") and prefixed ("user:entity_abc123").
fn user_id_from_sub(sub: &str) -> &str {
    sub.strip_prefix("user:").unwrap_or(sub)
}

fn is_owner_or_admin(schema: &SchemaDefinition, claims: &Claims, entity: &Entity) -> bool {
    if claims.has_role("admin") {
        return true;
    }
    let owner_field = match find_owner_field(schema) {
        Some(name) => name,
        None => return true,
    };
    let user_id = user_id_from_sub(&claims.sub);
    match entity.fields.get(&owner_field) {
        Some(DynamicValue::Text(s)) => s == user_id,
        _ => false,
    }
}
```

Update `OwnershipBasedPolicy::filter_visible`:

```rust
impl RecordAccessPolicy for OwnershipBasedPolicy {
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>> {
        Box::pin(async move {
            if claims.has_role("admin") {
                return entities;
            }
            let owner_field = match find_owner_field(schema) {
                Some(name) => name,
                None => return entities,
            };
            let user_id = user_id_from_sub(&claims.sub);
            entities
                .into_iter()
                .filter(|e| {
                    e.fields
                        .get(&owner_field)
                        .is_some_and(|val| matches!(val, DynamicValue::Text(s) if s == user_id))
                })
                .collect()
        })
    }

    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { is_owner_or_admin(schema, claims, entity) })
    }

    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { is_owner_or_admin(schema, claims, entity) })
    }
}
```

Keep `find_owner_field` unchanged. Remove `matches_user_id` (inlined above).

- [ ] **Step 4: Write complete test suite**

Full test module covering:
- `claims_has_role_returns_true_for_matching_role`
- `claims_has_role_returns_false_for_missing_role`
- `filter_visible_returns_all_for_admin`
- `filter_visible_filters_by_owner`
- `filter_visible_returns_all_when_no_owner_field`
- `can_modify_allows_owner`
- `can_modify_rejects_non_owner`
- `can_modify_allows_when_no_owner_annotation`
- `can_delete_allows_admin`
- `can_delete_denies_when_owner_field_missing_on_entity`
- `record_access_policy_is_object_safe`
- `user_id_from_sub_strips_prefix`
- `user_id_from_sub_passthrough_plain`

Each test constructs `Claims` directly. Owner-match tests use `make_claims_with_sub` where `sub` is set to match the entity's owner field value (with or without `user:` prefix).

- [ ] **Step 5: Run tests**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo nextest run -p schema-forge-backend
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/schema-forge-backend/
git commit -S -m "refactor(backend): replace AuthContext with acton-service Claims in RecordAccessPolicy"
```

---

## Task 3: Delete auth.rs and middleware.rs from schema-forge-acton

**Files:**
- Delete: `crates/schema-forge-acton/src/auth.rs`
- Delete: `crates/schema-forge-acton/src/middleware.rs`
- Modify: `crates/schema-forge-acton/src/lib.rs`

- [ ] **Step 1: Delete the files**

```bash
rm crates/schema-forge-acton/src/auth.rs crates/schema-forge-acton/src/middleware.rs
```

- [ ] **Step 2: Remove module declarations from lib.rs**

In `crates/schema-forge-acton/src/lib.rs`, remove these two lines:

```rust
pub mod auth;
```

```rust
pub mod middleware;
```

- [ ] **Step 3: Verify expected compilation errors**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check -p schema-forge-acton 2>&1 | head -40
```

Expected: errors in `extension.rs`, `state.rs`, `access.rs`, `routes/entities.rs`, `graphql/` about missing `auth`, `middleware`, `AuthProvider`, `OptionalAuth`, `AuthContext`. These are fixed in subsequent tasks.

- [ ] **Step 4: Commit (broken state — will be fixed in following tasks)**

```bash
git add crates/schema-forge-acton/src/auth.rs crates/schema-forge-acton/src/middleware.rs crates/schema-forge-acton/src/lib.rs
git commit -S -m "refactor(acton): delete custom AuthProvider and auth_middleware"
```

---

## Task 4: Update ForgeState and SchemaForgeExtensionBuilder

**Files:**
- Modify: `crates/schema-forge-acton/src/state.rs:283` — remove `auth_provider` field
- Modify: `crates/schema-forge-acton/src/extension.rs:33,52,81-86,211,242-245,272-275,328-331,350-353`

- [ ] **Step 1: Remove auth_provider from ForgeState**

In `crates/schema-forge-acton/src/state.rs`, remove:

```rust
    /// Optional auth provider for API request authentication.
    /// When `Some`, the auth middleware authenticates requests and injects
    /// [`AuthContext`](schema_forge_backend::auth::AuthContext) into extensions.
    /// When `None`, requests pass through without authentication (open access).
    pub auth_provider: Option<Arc<dyn crate::auth::AuthProvider>>,
```

- [ ] **Step 2: Remove auth_provider from SchemaForgeExtensionBuilder**

In `crates/schema-forge-acton/src/extension.rs`:

Remove the `auth_provider` field from the struct (line 33):
```rust
    auth_provider: Option<Arc<dyn crate::auth::AuthProvider>>,
```

Remove from `new()` (line 52):
```rust
            auth_provider: None,
```

Remove the `with_auth_provider` method entirely (lines 81-86):
```rust
    pub fn with_auth_provider<P: crate::auth::AuthProvider + 'static>(
        mut self,
        provider: P,
    ) -> Self {
        self.auth_provider = Some(Arc::new(provider));
        self
    }
```

Remove from `build()` (line 211):
```rust
            auth_provider: self.auth_provider,
```

- [ ] **Step 3: Remove auth middleware from all register_*_routes methods**

In `register_routes()` (around line 241-246), change:

```rust
        let forge_router = forge_routes()
            .route_layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                crate::middleware::auth_middleware,
            ))
            .with_state(self.state.clone());
```

to:

```rust
        let forge_router = forge_routes()
            .with_state(self.state.clone());
```

Apply the same change to:
- `register_versioned_routes()` (around line 271-276)
- `register_graphql_routes()` (around line 328-332) — if `#[cfg(feature = "graphql")]`
- `register_widget_routes()` (around line 350-354) — if `#[cfg(feature = "widget-ui")]`

Also update the doc comments that reference `auth_middleware` and `auth_provider`.

- [ ] **Step 4: Verify reduced error count**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check -p schema-forge-acton 2>&1 | head -40
```

Expected: errors should now only be in `access.rs`, `routes/entities.rs`, and `graphql/` about `OptionalAuth` and `AuthContext`.

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-acton/src/state.rs crates/schema-forge-acton/src/extension.rs
git commit -S -m "refactor(acton): remove auth_provider from ForgeState and extension builder"
```

---

## Task 5: Rewrite access.rs to use Claims

**Files:**
- Modify: `crates/schema-forge-acton/src/access.rs`

- [ ] **Step 1: Replace imports and OptionalAuth with OptionalClaims**

Replace the top of `crates/schema-forge-acton/src/access.rs`:

```rust
use std::collections::BTreeMap;

use acton_service::middleware::Claims;
use schema_forge_backend::entity::Entity;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_backend::TenantRef;
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{
    Annotation, DynamicValue, FieldAnnotation, FieldDefinition, SchemaDefinition,
};

use crate::error::ForgeError;

// ... (AccessAction and FieldFilterDirection enums stay the same)

/// Extractor that optionally extracts `Claims` from request extensions.
///
/// Returns `None` when no `Claims` are present (e.g. unauthenticated request
/// that passed through without token middleware).
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
```

- [ ] **Step 2: Update check_schema_access**

Change the signature from `auth: Option<&AuthContext>` to `claims: Option<&Claims>`:

```rust
pub fn check_schema_access(
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    action: AccessAction,
) -> Result<(), ForgeError> {
    // Rule 1: no claims means no auth middleware ran — deny
    let claims = match claims {
        Some(c) => c,
        None => {
            return Err(ForgeError::Unauthorized {
                message: "authentication required".to_string(),
            })
        }
    };

    // Rule 2: admin bypass
    if claims.has_role("admin") {
        return Ok(());
    }

    // Rule 3: no @access annotation = deny (secure by default)
    let (read_roles, write_roles, delete_roles) = match find_access_annotation(schema) {
        Some(roles) => roles,
        None => {
            return Err(ForgeError::Forbidden {
                message: format!(
                    "access denied: schema '{}' has no @access annotation (secure by default)",
                    schema.name.as_str(),
                ),
            })
        }
    };

    let required_roles = match action {
        AccessAction::Read => read_roles,
        AccessAction::Write => write_roles,
        AccessAction::Delete => delete_roles,
    };

    // Rule 4: "public" in role list = permit
    if required_roles.iter().any(|r| r == PUBLIC_ROLE) {
        return Ok(());
    }

    // Rule 5: empty role list = all authenticated users
    if required_roles.is_empty() {
        return Ok(());
    }

    // Rule 6: user must have at least one matching role
    if required_roles.iter().any(|r| claims.has_role(r)) {
        Ok(())
    } else {
        Err(ForgeError::Forbidden {
            message: format!(
                "access denied: user lacks required role for {:?} on schema '{}'",
                action,
                schema.name.as_str(),
            ),
        })
    }
}
```

**Key behavior change:** Rule 1 now **denies** when no Claims are present (secure by default). Previously it permitted (open access). This matches the spec's always-require-tokens design.

- [ ] **Step 3: Update filter_entity_fields**

```rust
pub fn filter_entity_fields(
    entity: &mut Entity,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    direction: FieldFilterDirection,
) {
    let claims = match claims {
        Some(c) => c,
        None => return, // No claims = no filtering (shouldn't happen with mandatory auth)
    };

    if claims.has_role("admin") {
        return;
    }

    let fields_to_remove: Vec<String> = entity
        .fields
        .keys()
        .filter(|field_name| {
            if let Some(field_def) = schema.field(field_name) {
                !is_field_accessible(field_def, &claims.roles, direction)
            } else {
                false
            }
        })
        .cloned()
        .collect();

    for name in fields_to_remove {
        entity.fields.remove(&name);
    }
}
```

- [ ] **Step 4: Update inject_tenant_scope**

```rust
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
    if claims.has_role("admin") {
        return;
    }
    let tenant_chain: Vec<TenantRef> = claims
        .custom_claim_as("tenant_chain")
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
```

- [ ] **Step 5: Update inject_tenant_on_create**

```rust
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
        .custom_claim_as("tenant_chain")
        .unwrap_or_default();
    if let Some(tenant_ref) = tenant_chain.last() {
        fields.insert(
            "_tenant".to_string(),
            DynamicValue::Text(tenant_ref.entity_id.clone()),
        );
    }
}
```

- [ ] **Step 6: Update test module**

Replace the test helpers. Key pattern for test Claims construction:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use acton_service::middleware::Claims;
    use schema_forge_core::types::{
        Annotation, EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType, SchemaId,
        SchemaName, TenantKind, TextConstraints,
    };
    use std::collections::{BTreeMap, HashMap};

    fn make_claims(roles: &[&str]) -> Claims {
        Claims {
            sub: format!("user:{}", EntityId::new().as_str()),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            perms: vec![],
            exp: 9999999999,
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
            serde_json::json!([{
                "schema": "Organization",
                "entity_id": tenant_entity_id
            }]),
        );
        claims
    }

    // ... all existing tests updated to use Claims instead of AuthContext
}
```

Every test that previously used `make_auth(roles)` uses `make_claims(roles)`.
Every test that used `make_auth_with_tenant(roles, entity_id)` uses `make_claims_with_tenant(roles, entity_id_str)`.

**Behavior change in tests:** `check_schema_access` with `None` claims now returns `Err(Unauthorized)` instead of `Ok`. Update the two tests:
- `check_schema_access_permits_when_no_auth_open_access` → `check_schema_access_denies_when_no_claims`
- `check_schema_access_permits_when_no_auth_no_annotation` → `check_schema_access_denies_when_no_claims_no_annotation`

- [ ] **Step 7: Run tests**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo nextest run -p schema-forge-acton --lib
```

Expected: access.rs tests PASS. Other compilation errors remain in routes/entities.rs and graphql/.

- [ ] **Step 8: Commit**

```bash
git add crates/schema-forge-acton/src/access.rs
git commit -S -m "refactor(acton): replace OptionalAuth/AuthContext with OptionalClaims/Claims in access control"
```

---

## Task 6: Remove AuthError from ForgeError

**Files:**
- Modify: `crates/schema-forge-acton/src/error.rs`

- [ ] **Step 1: Remove AuthError import and From impl**

In `crates/schema-forge-acton/src/error.rs`:

Remove the import:
```rust
use schema_forge_backend::auth::AuthError;
```

Remove the entire `From<AuthError>` impl block (lines ~171-184):
```rust
impl From<AuthError> for ForgeError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::MissingCredentials => Self::Unauthorized { ... },
            AuthError::InvalidCredentials { .. } => Self::Unauthorized { ... },
            AuthError::UserInactive { .. } => Self::Forbidden { ... },
            AuthError::Internal { message } => Self::Internal { message },
        }
    }
}
```

Remove the corresponding tests (lines ~480-505) that test `AuthError` conversion:
- `from_auth_error_missing_credentials`
- `from_auth_error_invalid_credentials`
- `from_auth_error_user_inactive`
- `from_auth_error_internal`

- [ ] **Step 2: Verify compilation**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check -p schema-forge-acton 2>&1 | head -20
```

- [ ] **Step 3: Run tests**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo nextest run -p schema-forge-acton -- error
```

Expected: remaining error tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/schema-forge-acton/src/error.rs
git commit -S -m "refactor(acton): remove AuthError conversion from ForgeError"
```

---

## Task 7: Update entity route handlers

**Files:**
- Modify: `crates/schema-forge-acton/src/routes/entities.rs`

- [ ] **Step 1: Update imports**

Replace:
```rust
use schema_forge_backend::auth::AuthContext;
```
with:
```rust
use acton_service::middleware::Claims;
```

Replace:
```rust
use crate::access::{
    check_schema_access, filter_entity_fields, inject_tenant_on_create, inject_tenant_scope,
    AccessAction, FieldFilterDirection, OptionalAuth,
};
```
with:
```rust
use crate::access::{
    check_schema_access, filter_entity_fields, inject_tenant_on_create, inject_tenant_scope,
    AccessAction, FieldFilterDirection, OptionalClaims,
};
```

- [ ] **Step 2: Update execute_entity_query**

Change `auth: Option<&AuthContext>` to `claims: Option<&Claims>`:

```rust
async fn execute_entity_query(
    state: &ForgeState,
    schema_def: &SchemaDefinition,
    claims: Option<&Claims>,
    query: &mut schema_forge_core::query::Query,
) -> Result<ListEntitiesResponse, ForgeError> {
    inject_tenant_scope(query, claims, &state.tenant_config);

    let result = state.backend.query(query).await.map_err(ForgeError::from)?;

    let visible_entities =
        if let (Some(ref policy), Some(c)) = (&state.record_access_policy, claims) {
            policy.filter_visible(schema_def, c, result.entities).await
        } else {
            result.entities
        };

    let entities: Vec<EntityResponse> = visible_entities
        .into_iter()
        .map(|mut e| {
            filter_entity_fields(&mut e, schema_def, claims, FieldFilterDirection::Read);
            entity_to_response(&e)
        })
        .collect();
    let count = entities.len();

    Ok(ListEntitiesResponse {
        entities,
        count,
        total_count: result.total_count,
    })
}
```

- [ ] **Step 3: Update all 6 handler functions**

In each handler, replace `OptionalAuth(auth): OptionalAuth` with `OptionalClaims(claims): OptionalClaims`, and replace every `auth` usage with `claims`:

Handlers to update:
1. `create_entity` — `OptionalClaims(claims)`, `claims.as_ref()` in all access calls
2. `list_entities` — same pattern
3. `query_entities` — same pattern
4. `get_entity` — same pattern, plus update record-level check:
   ```rust
   if let (Some(ref policy), Some(ref c)) = (&state.record_access_policy, &claims) {
       let visible = policy.filter_visible(&schema_def, c, vec![entity.clone()]).await;
   ```
5. `update_entity` — same pattern, plus update record-level check:
   ```rust
   if let (Some(ref policy), Some(ref c)) = (&state.record_access_policy, &claims) {
       if !policy.can_modify(&schema_def, c, &existing).await {
   ```
6. `delete_entity` — same pattern, plus update record-level check:
   ```rust
   if let (Some(ref policy), Some(ref c)) = (&state.record_access_policy, &claims) {
       if !policy.can_delete(&schema_def, c, &entity).await {
   ```

- [ ] **Step 4: Verify compilation**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check -p schema-forge-acton 2>&1 | head -20
```

Expected: remaining errors only in `graphql/` modules. Entity routes should compile.

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-acton/src/routes/entities.rs
git commit -S -m "refactor(acton): use OptionalClaims in all entity route handlers"
```

---

## Task 8: Update GraphQL module

**Files:**
- Modify: `crates/schema-forge-acton/src/graphql/context.rs`
- Modify: `crates/schema-forge-acton/src/graphql/mod.rs`
- Modify: `crates/schema-forge-acton/src/graphql/resolvers.rs`

- [ ] **Step 1: Update context.rs**

Replace entire file:

```rust
use acton_service::middleware::Claims;

use crate::state::ForgeState;

/// Request-scoped context inserted into every async-graphql request via `.data()`.
///
/// Resolvers access it with `ctx.data::<ForgeGraphqlContext>()`.
pub struct ForgeGraphqlContext {
    pub state: ForgeState,
    pub claims: Option<Claims>,
}
```

- [ ] **Step 2: Update mod.rs handler**

In `crates/schema-forge-acton/src/graphql/mod.rs`, change:
- `OptionalAuth(auth): OptionalAuth` → `OptionalClaims(claims): OptionalClaims`
- `auth,` → `claims,` in the context construction
- Import `OptionalClaims` instead of `OptionalAuth`

- [ ] **Step 3: Update resolvers.rs**

In `crates/schema-forge-acton/src/graphql/resolvers.rs`:
- `let auth = gql_ctx.auth.as_ref();` → `let claims = gql_ctx.claims.as_ref();`
- Every `auth` reference in access calls → `claims`
- Every `auth_ctx` in record-level checks → `c` (matching the Claims variable)

For each resolver function:
```rust
// Pattern change
let claims = gql_ctx.claims.as_ref();
check_schema_access(schema_def, claims, AccessAction::Read).map_err(forge_error_to_gql)?;
// ...
if let (Some(ref policy), Some(c)) = (&gql_ctx.state.record_access_policy, claims) {
    policy.filter_visible(schema_def, c, ...)
```

- [ ] **Step 4: Verify full compilation**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check -p schema-forge-acton
```

Expected: PASS — all source code compiles. Test compilation may still fail (test files reference old types).

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-acton/src/graphql/
git commit -S -m "refactor(acton): use Claims in GraphQL context and resolvers"
```

---

## Task 9: Update integration and auth_demo tests

**Files:**
- Modify: `crates/schema-forge-acton/tests/integration.rs`
- Modify: `crates/schema-forge-acton/tests/auth_demo.rs`
- Modify: `crates/schema-forge-acton/tests/admin_integration.rs`

- [ ] **Step 1: Update integration.rs test helpers**

Replace `test_state()` — remove `auth_provider: None`:

```rust
async fn test_state() -> ForgeState {
    let backend = SurrealBackend::connect_memory("test", "test")
        .await
        .expect("failed to connect to in-memory SurrealDB");
    let registry = SchemaRegistry::new();
    ForgeState {
        registry,
        backend: Arc::new(backend),
        tenant_config: None,
        record_access_policy: None,
        #[cfg(feature = "graphql")]
        graphql_schema: schema_forge_acton::graphql::empty_graphql_schema(),
        #[cfg(any(feature = "admin-ui", feature = "widget-ui"))]
        surreal_client: None,
        #[cfg(any(feature = "admin-ui", feature = "widget-ui"))]
        template_engine: std::sync::Arc::new(
            schema_forge_acton::template_engine::TemplateEngine::new(
                Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")),
            ),
        ),
    }
}
```

Replace `test_app_with_state()` — remove auth middleware, add optional Claims injection:

```rust
fn test_app_with_state(state: ForgeState) -> Router {
    forge_routes().with_state(state)
}

/// Create a test router that injects specific Claims into every request.
fn test_app_with_claims(state: ForgeState, claims: Claims) -> Router {
    forge_routes()
        .layer(axum::middleware::from_fn(move |mut req: axum::extract::Request, next: axum::middleware::Next| {
            let claims = claims.clone();
            async move {
                req.extensions_mut().insert(claims);
                next.run(req).await
            }
        }))
        .with_state(state)
}
```

Add Claims helper:

```rust
use acton_service::middleware::Claims;
use std::collections::HashMap;

fn make_test_claims(roles: &[&str]) -> Claims {
    Claims {
        sub: "user:test-user".to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        perms: vec![],
        exp: 9999999999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    }
}
```

Remove `FailingAuthProvider`, `NoopAuthProvider` imports and usages. Tests that used `auth_provider: Some(Arc::new(NoopAuthProvider::admin()))` now use `test_app_with_claims(state, make_test_claims(&["admin"]))`.

Tests that tested "failing auth" now test "no Claims in extensions" — the request should get 401.

- [ ] **Step 2: Update admin_integration.rs**

Remove `auth_provider: None` from `admin_test_state()`.

- [ ] **Step 3: Rewrite auth_demo.rs**

This is the largest test file change. Remove `ConfigurableAuthProvider` entirely. Replace with Claims-injection pattern:

Remove old imports:
```rust
use schema_forge_acton::auth::{AuthProvider, NoopAuthProvider};
use schema_forge_backend::auth::{AuthContext, AuthError, OwnershipBasedPolicy, TenantRef};
```

Add new imports:
```rust
use acton_service::middleware::Claims;
use schema_forge_backend::{OwnershipBasedPolicy, TenantRef};
```

Replace `test_app_with_state()`:
```rust
fn test_app_with_state(state: ForgeState) -> Router {
    forge_routes().with_state(state)
}

fn test_app_with_claims(state: ForgeState, claims: Claims) -> Router {
    forge_routes()
        .layer(axum::middleware::from_fn(move |mut req: axum::extract::Request, next: axum::middleware::Next| {
            let claims = claims.clone();
            async move {
                req.extensions_mut().insert(claims);
                next.run(req).await
            }
        }))
        .with_state(state)
}
```

Every test that previously set `auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["editor".into()])))` in ForgeState now:
1. Removes `auth_provider` from ForgeState
2. Uses `test_app_with_claims(state, make_claims(&["editor"]))` instead of `test_app_with_state(state)`

For tenant tests, use `make_claims_with_tenant`:

```rust
fn make_claims_with_tenant(sub: &str, roles: &[&str], tenant_entity_id: &str) -> Claims {
    let mut claims = Claims {
        sub: sub.to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        perms: vec![],
        exp: 9999999999,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    };
    claims.custom.insert(
        "tenant_chain".to_string(),
        serde_json::json!([{
            "schema": "Organization",
            "entity_id": tenant_entity_id
        }]),
    );
    claims
}
```

- [ ] **Step 4: Run all tests**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo nextest run -p schema-forge-acton
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-acton/tests/
git commit -S -m "test(acton): migrate all tests from AuthContext/AuthProvider to Claims"
```

---

## Task 10: Update schema-forge-cli serve command

**Files:**
- Modify: `crates/schema-forge-cli/src/commands/serve.rs`

- [ ] **Step 1: Remove with_auth_provider if present**

Check if `serve.rs` uses `with_auth_provider` anywhere. Based on current code, it doesn't — the extension builder doesn't call it. Verify:

```bash
grep -n "auth_provider\|with_auth_provider\|AuthProvider" crates/schema-forge-cli/src/commands/serve.rs
```

Expected: no matches. If there are matches, remove them.

- [ ] **Step 2: Verify full workspace compilation**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo check
```

Expected: PASS — entire workspace compiles.

- [ ] **Step 3: Commit (if changes were needed)**

Only commit if there were changes to serve.rs.

---

## Task 11: Run clippy and full test suite

**Files:** None (verification only)

- [ ] **Step 1: Run clippy**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo clippy --all-targets --all-features 2>&1
```

Expected: 0 warnings. Fix any that appear.

- [ ] **Step 2: Run full test suite**

```bash
cd /home/rodzilla/code/active/schemaforge && cargo nextest run
```

Expected: all tests PASS.

- [ ] **Step 3: Commit any clippy fixes**

```bash
git add -A && git commit -S -m "fix: address clippy warnings from auth refactor"
```

Only if there were fixes.

- [ ] **Step 4: Final verification commit message**

If all tasks committed cleanly, no action needed. The refactor is complete.

---

## Deferred Work (separate plan)

The following spec items are new features that extend beyond the refactor and should be planned separately:

- **`schema-forge dev-token` CLI command** — mints test PASETO tokens with configurable `--roles`, `--sub`, `--tenant-chain`. Requires enabling the `auth` feature on acton-service (for `PasetoGenerator`).
- **`schema-forge init` config.local.toml generation** — generates a dev PASETO key and token config section during project initialization.
