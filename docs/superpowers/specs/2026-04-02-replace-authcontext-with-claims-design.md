# Replace AuthContext with acton-service Claims

**Date:** 2026-04-02
**Status:** Approved
**Scope:** schema-forge-backend, schema-forge-acton, schema-forge-cli

## Summary

Remove SchemaForge's custom `AuthContext`, `AuthProvider` trait, and `auth_middleware` in favor of acton-service's built-in `Claims` type and `PasetoAuth` middleware. acton-service is the foundational framework — auth should flow through it directly, not through a parallel abstraction.

## Motivation

SchemaForge currently maintains a parallel auth stack:

- `AuthProvider` trait (custom) — duplicates what `PasetoAuth`/`JwtAuth` already do
- `auth_middleware` (custom) — duplicates acton-service's token validation middleware
- `AuthContext` (custom) — duplicates `Claims` with extra fields that can be custom claims
- `NoopAuthProvider` — exists only because the custom trait requires an implementation for dev mode

acton-service v0.21 added custom claims support (`Claims.custom` with `#[serde(flatten)]`), eliminating the last reason for a separate auth type. Tenant chain and attributes fit naturally as custom PASETO claims.

## Token Payload Structure

```json
{
  "sub": "user:abc-123",
  "roles": ["editor", "hr"],
  "perms": ["schema:read", "entity:write"],
  "exp": 1735689599,
  "iat": 1735603199,
  "jti": "tok_unique_id",
  "iss": "schema-forge",
  "tenant_chain": [
    { "schema": "Organization", "entity_id": "org-42" }
  ]
}
```

- `sub`, `roles`, `perms`, `exp`, `iat`, `jti`, `iss` — standard `Claims` fields
- `tenant_chain` — custom claim, deserialized via `claims.custom_claim_as::<Vec<TenantRef>>()`

## Deletions

| File | Removed |
|------|---------|
| `schema-forge-acton/src/auth.rs` | Entire file: `AuthProvider` trait, `NoopAuthProvider`, all tests |
| `schema-forge-acton/src/middleware.rs` | Entire file: `auth_middleware` function |
| `schema-forge-backend/src/auth.rs` | `AuthContext` struct, `AuthError` enum, `TenantRef` (moved — see below) |
| `schema-forge-acton/src/access.rs` | `OptionalAuth` extractor |
| `schema-forge-acton/src/lib.rs` | `pub mod auth;` and `pub mod middleware;` declarations |

## New Dependency

**`schema-forge-backend/Cargo.toml`** adds acton-service with no features:

```toml
acton-service = { version = "0.21", default-features = false }
```

Only the `Claims` type is needed. It lives in `acton_service::middleware::token::Claims` and is available without any feature flags.

## Rewritten Modules

### schema-forge-backend/src/auth.rs

**Removed:** `AuthContext`, `AuthError`, `TenantRef` as standalone types.

**Kept:** `RecordAccessPolicy` trait, `OwnershipBasedPolicy` implementation, `find_owner_field()`, `matches_user_id()`, `is_owner_or_admin()`.

**Changed signatures:**

```rust
// Before
pub fn filter_visible(&self, schema: &SchemaDefinition, auth: &AuthContext, entities: Vec<Entity>) -> ...
pub fn can_modify(&self, schema: &SchemaDefinition, auth: &AuthContext, entity: &Entity) -> ...
pub fn can_delete(&self, schema: &SchemaDefinition, auth: &AuthContext, entity: &Entity) -> ...

// After
pub fn filter_visible(&self, schema: &SchemaDefinition, claims: &Claims, entities: Vec<Entity>) -> ...
pub fn can_modify(&self, schema: &SchemaDefinition, claims: &Claims, entity: &Entity) -> ...
pub fn can_delete(&self, schema: &SchemaDefinition, claims: &Claims, entity: &Entity) -> ...
```

**Field mapping:**

| AuthContext field | Claims equivalent |
|---|---|
| `user_id: EntityId` | `claims.sub` (String) |
| `roles: Vec<String>` | `claims.roles` (Vec<String>) |
| `is_admin()` | `claims.has_role("admin")` |
| `has_role(r)` | `claims.has_role(r)` |
| `has_any_role(rs)` | `rs.iter().any(\|r\| claims.has_role(r))` |
| `tenant_chain: Vec<TenantRef>` | `claims.custom_claim_as::<Vec<TenantRef>>("tenant_chain")` |
| `attributes: BTreeMap<String, String>` | Individual custom claims accessed by key |

**`TenantRef`** moves to `schema-forge-backend/src/tenant.rs` (alongside `TenantConfig`) with `Serialize`/`Deserialize` derives so it can round-trip through PASETO custom claims:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRef {
    pub schema: String,     // Was SchemaName — simplified for claim serialization
    pub entity_id: String,  // Was EntityId — simplified for claim serialization
}
```

### schema-forge-acton/src/access.rs

**`OptionalAuth` extractor removed.** Replaced by a new `OptionalClaims` extractor that reads `Claims` from request extensions:

```rust
pub struct OptionalClaims(pub Option<Claims>);

impl<S> FromRequestParts<S> for OptionalClaims
where S: Send + Sync {
    type Rejection = Infallible;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(OptionalClaims(parts.extensions.get::<Claims>().cloned()))
    }
}
```

**All access functions change `Option<&AuthContext>` to `Option<&Claims>`:**

- `check_schema_access(schema, claims, action)` — role checking uses `claims.has_role()` / `claims.roles`
- `filter_entity_fields(entity, schema, claims, direction)` — same logic, different type
- `inject_tenant_scope(query, claims, tenant_config)` — extracts tenant from `claims.custom_claim_as::<Vec<TenantRef>>("tenant_chain")`
- `inject_tenant_on_create(fields, claims, tenant_config)` — same extraction

### schema-forge-acton/src/state.rs

**`ForgeState` loses `auth_provider`:**

```rust
pub struct ForgeState {
    pub registry: SchemaRegistry,
    pub backend: Arc<dyn DynForgeBackend>,
    // REMOVED: pub auth_provider: Option<Arc<dyn crate::auth::AuthProvider>>,
    pub tenant_config: Option<TenantConfig>,
    pub record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
    // ... feature-gated fields unchanged
}
```

### schema-forge-acton/src/extension.rs

**`SchemaForgeExtensionBuilder` loses `with_auth_provider()` and `auth_provider` field.**

**`register_*_routes()` methods lose `auth_middleware` route layer.** Token validation is handled globally by acton-service's `ServiceBuilder` when `[token]` config is present. SchemaForge routes no longer apply their own auth middleware.

```rust
// Before
pub fn register_versioned_routes<T>(&self, router: Router<AppState<T>>) -> Router<AppState<T>> {
    let forge_router: Router<()> = forge_routes()
        .route_layer(axum::middleware::from_fn_with_state(
            self.state.clone(),
            crate::middleware::auth_middleware,
        ))
        .with_state(self.state.clone());
    router.nest_service("/forge", forge_router)
}

// After
pub fn register_versioned_routes<T>(&self, router: Router<AppState<T>>) -> Router<AppState<T>> {
    let forge_router: Router<()> = forge_routes()
        .with_state(self.state.clone());
    router.nest_service("/forge", forge_router)
}
```

Same change for `register_routes()`, `register_graphql_routes()`, and `register_widget_routes()`.

### schema-forge-acton/src/routes/entities.rs

All handlers change `OptionalAuth(auth)` to `OptionalClaims(claims)`:

```rust
// Before
pub async fn create_entity(
    State(state): State<ForgeState>,
    Path(schema): Path<String>,
    OptionalAuth(auth): OptionalAuth,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)?;
    inject_tenant_on_create(&mut fields, auth.as_ref(), &state.tenant_config);

// After
pub async fn create_entity(
    State(state): State<ForgeState>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    check_schema_access(&schema_def, claims.as_ref(), AccessAction::Write)?;
    inject_tenant_on_create(&mut fields, claims.as_ref(), &state.tenant_config);
```

### schema-forge-acton/src/graphql/context.rs

```rust
// Before
pub struct ForgeGraphqlContext {
    pub state: ForgeState,
    pub auth: Option<AuthContext>,
}

// After
pub struct ForgeGraphqlContext {
    pub state: ForgeState,
    pub claims: Option<Claims>,
}
```

### schema-forge-acton/src/lib.rs

Remove `pub mod auth;` and `pub mod middleware;` declarations.

### schema-forge-backend/src/lib.rs

Remove `AuthContext`, `AuthError`, `TenantRef` from pub exports. Keep `RecordAccessPolicy`, `OwnershipBasedPolicy`.

## Secure-by-Default Behavior

**Before:** No `auth_provider` configured = open access (all requests pass through).

**After:** No `[token]` config section = server refuses to start with an explicit error message. Auth is mandatory.

- `schema-forge init` generates `config.local.toml` with a dev PASETO symmetric key
- New CLI command `schema-forge dev-token` mints test tokens with configurable `--roles`, `--sub`, `--tenant-chain`

## Test Strategy

### Unit tests (schema-forge-backend)

`RecordAccessPolicy` and `OwnershipBasedPolicy` tests change from constructing `AuthContext` to constructing `Claims`:

```rust
fn make_claims(roles: &[&str]) -> Claims {
    Claims {
        sub: format!("user:{}", EntityId::new()),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        perms: vec![],
        exp: 9999999999,
        ..Default::default()
    }
}
```

### Unit tests (schema-forge-acton/src/access.rs)

Same pattern — construct `Claims` directly. Tenant tests use `custom` field:

```rust
fn make_claims_with_tenant(roles: &[&str], tenant_entity_id: &str) -> Claims {
    let mut claims = make_claims(roles);
    claims.custom.insert(
        "tenant_chain".to_string(),
        serde_json::json!([{ "schema": "Organization", "entity_id": tenant_entity_id }]),
    );
    claims
}
```

### Integration tests (auth_demo.rs, integration.rs)

The `ConfigurableAuthProvider` and `FailingAuthProvider` test structs are deleted. Tests that need authenticated requests inject `Claims` directly into request extensions via a test middleware helper:

```rust
/// Test middleware that injects Claims into request extensions.
fn with_test_claims(claims: Claims) -> axum::middleware::FromFnLayer<...> {
    axum::middleware::from_fn(move |mut req: Request, next: Next| {
        let claims = claims.clone();
        async move {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
    })
}
```

Tests that previously tested "auth provider returns error" become tests that verify "missing Claims in extensions = 401".

### ForgeState construction in tests

```rust
// Before
ForgeState {
    auth_provider: Some(Arc::new(NoopAuthProvider::new(vec!["editor".into()]))),
    ...
}

// After — no auth_provider field, Claims injected via middleware layer
ForgeState {
    // auth_provider removed
    ...
}
```

## Unchanged Code

- `check_schema_access` logic (role matching against `@access` annotations)
- `filter_entity_fields` logic (field-level access filtering)
- `inject_tenant_scope` / `inject_tenant_on_create` logic (tenant scoping)
- `RecordAccessPolicy` / `OwnershipBasedPolicy` (record-level ownership)
- Cedar policy generation (`cedar.rs`)
- Schema DSL, parsing, migration, backend code
- Admin UI and widget UI session auth (separate concern)

## Migration Checklist

1. Add `acton-service` dependency to `schema-forge-backend`
2. Rewrite `schema-forge-backend/src/auth.rs` — remove `AuthContext`/`AuthError`, update `RecordAccessPolicy` to use `Claims`
3. Update `schema-forge-backend/src/lib.rs` exports
4. Delete `schema-forge-acton/src/auth.rs`
5. Delete `schema-forge-acton/src/middleware.rs`
6. Update `schema-forge-acton/src/lib.rs` — remove module declarations
7. Rewrite `schema-forge-acton/src/access.rs` — `OptionalClaims`, update all functions
8. Update `schema-forge-acton/src/state.rs` — remove `auth_provider`
9. Update `schema-forge-acton/src/extension.rs` — remove `with_auth_provider`, remove auth middleware layers
10. Update `schema-forge-acton/src/routes/entities.rs` — `OptionalClaims` in all handlers
11. Update `schema-forge-acton/src/graphql/context.rs`
12. Update all tests in `schema-forge-backend`
13. Update all tests in `schema-forge-acton` (unit + integration)
14. Add `schema-forge dev-token` CLI command
15. Update `schema-forge init` to generate `config.local.toml` with dev PASETO key
16. Verify: `cargo clippy` 0 warnings, `cargo nextest run` 0 failures
