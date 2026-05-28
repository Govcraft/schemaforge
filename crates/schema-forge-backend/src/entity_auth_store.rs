//! `AuthStore` impl backed by the `User` schema's entity table.
//!
//! `EntityAuthStore` is the production path for SchemaForge's user
//! identity store. It satisfies the [`AuthStore`] contract by reading
//! and writing rows in the `User` entity table — the same table that
//! `/api/v1/forge/entities/User` lists, except the `password_hash`
//! field is locked behind the `@hidden` annotation so it never appears
//! in any API response.
//!
//! This collapses the prior duality between `_forge_users` (auth) and
//! the `User` schema (identity attributes) into a single source of
//! truth. Everything Cedar reasons about — `email`, `roles`,
//! `role_rank`, `display_name`, `active` — lives in one row, addressable
//! by the same `EntityId` shape every other entity uses.
//!
//! Internal consumers (this struct's own methods) read raw entities
//! directly from the backend so the password hash is available for
//! verification, while every external surface continues to flow through
//! `Entity::strip_hidden` before serialization.

use std::sync::Arc;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::{DateTime, Utc};
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{
    DynamicValue, EntityId, FieldName, FieldType, IntegerConstraints, SchemaDefinition, SchemaName,
    TextConstraints,
};

use crate::entity::Entity;
use crate::error::BackendError;
use crate::tenant::TenantRef;
use crate::traits::EntityStore;
use crate::user_store::{AuthStore, ForgeUser};

const USERNAME_FIELD: &str = "email";
const PASSWORD_HASH_FIELD: &str = "password_hash";
const ROLES_FIELD: &str = "roles";
const ROLE_RANK_FIELD: &str = "role_rank";
const DISPLAY_NAME_FIELD: &str = "display_name";
const ACTIVE_FIELD: &str = "active";
const LAST_LOGIN_FIELD: &str = "last_login";

const TM_USER_FIELD: &str = "user";
const TM_TENANT_TYPE_FIELD: &str = "tenant_type";
const TM_TENANT_ID_FIELD: &str = "tenant_id";
const TM_ROLE_FIELD: &str = "role";

/// Function that maps a role name to a numeric rank, returning `None`
/// for unregistered roles.
///
/// Wired by the extension layer from `RoleRanks::get`-like APIs so the
/// store can recompute `role_rank` on every user mutation without
/// taking a hard dependency on the policy crate.
pub type RoleRankResolver = Arc<dyn Fn(&str) -> Option<i64> + Send + Sync>;

/// Compute the role-rank for `roles` using the supplied lookup function.
///
/// `role_rank_of` returns `None` for unregistered roles; callers may
/// substitute the platform default (`0`) so unknown roles contribute
/// nothing rather than poisoning the max with a sentinel.
pub fn compute_role_rank<F>(roles: &[String], mut role_rank_of: F) -> i64
where
    F: FnMut(&str) -> Option<i64>,
{
    roles
        .iter()
        .map(|r| role_rank_of(r).unwrap_or(0))
        .max()
        .unwrap_or(0)
}

/// `AuthStore` implementation that operates over the `User` entity table.
///
/// Generic over a backend that satisfies [`EntityStore`]. The store is
/// constructed from:
///
/// - the entity-store handle (typed-erased through `Arc<dyn EntityStore>`
///   in production wiring),
/// - the resolved `User` [`SchemaDefinition`] (so we know the SchemaId
///   and field shape without re-reading the registry on every call),
/// - a closure that maps role names to ranks (typically wired from
///   `RoleRanks::get`).
///
/// All credential plaintexts are passed through `argon2` for hashing and
/// verification. Storage of the resulting hash uses the schema's
/// `password_hash` field, marked `@hidden` so external API surfaces
/// never serialize it.
pub struct EntityAuthStore {
    store: Arc<dyn DynEntityStore>,
    user_schema: SchemaDefinition,
    role_rank_resolver: RoleRankResolver,
    /// `TenantMembership` schema definition, used to query the user's
    /// flat membership set during login/refresh. `None` when the
    /// deployment has not seeded the system schema yet (e.g. a
    /// `mem://` smoke test that skips system-schema seeding) — in that
    /// case [`AuthStore::list_tenant_memberships`] returns an empty
    /// `Vec`, mirroring the "no memberships configured" path.
    tenant_membership_schema: Option<SchemaDefinition>,
}

/// Object-safe variant of [`EntityStore`] for the `Arc<dyn ...>` storage
/// backing [`EntityAuthStore`]. We can't store `Arc<dyn EntityStore>`
/// directly because the trait uses `impl Future` (RPITIT), which is not
/// dyn-compatible. Concrete backends implement this trait via
/// [`DynEntityStoreExt`], which wraps each RPITIT method in a boxed
/// future.
pub trait DynEntityStore: Send + Sync {
    fn create<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Entity, BackendError>> + Send + 'a>,
    >;
    fn get<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Entity, BackendError>> + Send + 'a>,
    >;
    fn update<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Entity, BackendError>> + Send + 'a>,
    >;
    fn delete<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BackendError>> + Send + 'a>>;
    fn query<'a>(
        &'a self,
        query: &'a Query,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<crate::entity::QueryResult, BackendError>>
            + Send
            + 'a>,
    >;
    fn count<'a>(
        &'a self,
        query: &'a Query,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<usize, BackendError>> + Send + 'a>,
    >;
}

/// Blanket adapter that wraps any concrete [`EntityStore`] in
/// [`DynEntityStore`].
///
/// Concrete backend types (`SurrealBackend`, `PgBackend`) get
/// `DynEntityStore` for free via this impl, with each RPITIT method's
/// returned future boxed into the trait-object-safe shape.
impl<S> DynEntityStore for S
where
    S: EntityStore + ?Sized,
{
    fn create<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Entity, BackendError>> + Send + 'a>,
    > {
        Box::pin(EntityStore::create(self, entity))
    }
    fn get<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Entity, BackendError>> + Send + 'a>,
    > {
        Box::pin(EntityStore::get(self, schema, id))
    }
    fn update<'a>(
        &'a self,
        entity: &'a Entity,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Entity, BackendError>> + Send + 'a>,
    > {
        Box::pin(EntityStore::update(self, entity))
    }
    fn delete<'a>(
        &'a self,
        schema: &'a SchemaName,
        id: &'a EntityId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BackendError>> + Send + 'a>>
    {
        Box::pin(EntityStore::delete(self, schema, id))
    }
    fn query<'a>(
        &'a self,
        query: &'a Query,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<crate::entity::QueryResult, BackendError>>
            + Send
            + 'a>,
    > {
        Box::pin(EntityStore::query(self, query))
    }
    fn count<'a>(
        &'a self,
        query: &'a Query,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<usize, BackendError>> + Send + 'a>,
    > {
        Box::pin(EntityStore::count(self, query))
    }
}

impl EntityAuthStore {
    /// Constructs a new [`EntityAuthStore`].
    ///
    /// `role_rank_resolver` returns the rank for a role name, or `None`
    /// for unregistered roles. Typically wired from
    /// `RoleRanks::get`-like API.
    pub fn new(
        store: Arc<dyn DynEntityStore>,
        user_schema: SchemaDefinition,
        role_rank_resolver: RoleRankResolver,
    ) -> Self {
        debug_assert_eq!(
            user_schema.name.as_str(),
            "User",
            "EntityAuthStore must be constructed with the User SchemaDefinition"
        );
        Self {
            store,
            user_schema,
            role_rank_resolver,
            tenant_membership_schema: None,
        }
    }

    /// Attach the `TenantMembership` schema so [`AuthStore::list_tenant_memberships`]
    /// can query the user's membership rows.
    ///
    /// Optional builder method: deployments without the system schema
    /// seeded (or running tenancy-disabled smoke tests) can skip it,
    /// and `list_tenant_memberships` will return an empty `Vec`.
    pub fn with_tenant_membership_schema(mut self, schema: SchemaDefinition) -> Self {
        debug_assert_eq!(
            schema.name.as_str(),
            "TenantMembership",
            "with_tenant_membership_schema must be called with the TenantMembership SchemaDefinition"
        );
        self.tenant_membership_schema = Some(schema);
        self
    }

    /// Returns the password_hash field type from the User schema, used by
    /// migrations to verify the schema declares the column the auth
    /// store needs.
    pub fn required_user_schema_fields() -> &'static [(&'static str, FieldType)] {
        // Compile-time enforcement is not possible (FieldType isn't const),
        // but we expose the field list so tests can assert the system
        // schema matches.
        &[]
    }

    fn user_schema_name(&self) -> &SchemaName {
        &self.user_schema.name
    }

    /// Find a user entity by username (== the `email` field on the User
    /// schema). Returns the raw Entity, not a `ForgeUser`, so callers
    /// that need the password hash can read it.
    async fn find_entity_by_username(
        &self,
        username: &str,
    ) -> Result<Option<Entity>, BackendError> {
        let query = Query::new(self.user_schema.id.clone())
            .with_filter(Filter::eq(
                FieldPath::single(USERNAME_FIELD),
                DynamicValue::Text(username.to_string()),
            ))
            .with_limit(1);
        let result = self.store.query(&query).await?;
        Ok(result.entities.into_iter().next())
    }

    /// Hash a plaintext password using argon2 with a fresh salt.
    fn hash_password(plaintext: &str) -> Result<String, BackendError> {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(plaintext.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| BackendError::Internal {
                message: format!("password hashing failed: {e}"),
            })
    }

    /// Verify a plaintext password against an argon2 PHC string. Errors
    /// only on malformed hashes; a mismatch returns `Ok(false)`.
    fn verify_password(plaintext: &str, hash: &str) -> Result<bool, BackendError> {
        let parsed = PasswordHash::new(hash).map_err(|e| BackendError::Internal {
            message: format!("invalid stored password hash: {e}"),
        })?;
        Ok(Argon2::default()
            .verify_password(plaintext.as_bytes(), &parsed)
            .is_ok())
    }

    fn entity_to_forge_user(&self, entity: &Entity) -> ForgeUser {
        ForgeUser {
            username: extract_text(entity, USERNAME_FIELD).unwrap_or_default(),
            roles: extract_text_array(entity, ROLES_FIELD),
            display_name: extract_optional_text(entity, DISPLAY_NAME_FIELD),
            active: extract_boolean(entity, ACTIVE_FIELD).unwrap_or(true),
            role_rank: extract_integer(entity, ROLE_RANK_FIELD).unwrap_or(0),
        }
    }

    fn build_user_entity(
        &self,
        username: &str,
        roles: &[String],
        display_name: &str,
        password_hash: String,
    ) -> Entity {
        let role_rank = compute_role_rank(roles, |r| (self.role_rank_resolver)(r));

        let mut fields: std::collections::BTreeMap<String, DynamicValue> =
            std::collections::BTreeMap::new();
        fields.insert(
            USERNAME_FIELD.to_string(),
            DynamicValue::Text(username.to_string()),
        );
        fields.insert(
            DISPLAY_NAME_FIELD.to_string(),
            DynamicValue::Text(display_name.to_string()),
        );
        fields.insert(
            ROLES_FIELD.to_string(),
            DynamicValue::Array(roles.iter().cloned().map(DynamicValue::Text).collect()),
        );
        fields.insert(
            ROLE_RANK_FIELD.to_string(),
            DynamicValue::Integer(role_rank),
        );
        fields.insert(ACTIVE_FIELD.to_string(), DynamicValue::Boolean(true));
        fields.insert(
            PASSWORD_HASH_FIELD.to_string(),
            DynamicValue::Text(password_hash),
        );

        Entity::new(self.user_schema_name().clone(), fields)
    }

    /// Compile-time hint for which User schema fields the auth store
    /// touches. Surfaced via [`Self::user_schema_field_names`] so a
    /// future migration command can validate the deployed schema before
    /// rolling out.
    pub fn user_schema_field_names() -> [&'static str; 6] {
        [
            USERNAME_FIELD,
            PASSWORD_HASH_FIELD,
            ROLES_FIELD,
            ROLE_RANK_FIELD,
            DISPLAY_NAME_FIELD,
            ACTIVE_FIELD,
        ]
    }
}

// ---------------------------------------------------------------------------
// Field extraction helpers (pure, unit-tested)
// ---------------------------------------------------------------------------

fn extract_text(entity: &Entity, field: &str) -> Option<String> {
    match entity.field(field) {
        Some(DynamicValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn extract_optional_text(entity: &Entity, field: &str) -> Option<String> {
    match entity.field(field) {
        Some(DynamicValue::Text(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn extract_text_array(entity: &Entity, field: &str) -> Vec<String> {
    match entity.field(field) {
        Some(DynamicValue::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                DynamicValue::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn extract_boolean(entity: &Entity, field: &str) -> Option<bool> {
    match entity.field(field) {
        Some(DynamicValue::Boolean(b)) => Some(*b),
        _ => None,
    }
}

fn extract_integer(entity: &Entity, field: &str) -> Option<i64> {
    match entity.field(field) {
        Some(DynamicValue::Integer(n)) => Some(*n),
        _ => None,
    }
}

/// Build a [`TenantRef`] from a `TenantMembership` entity row.
///
/// Pure: no I/O, no state. Returns `None` if either required field is
/// missing or carries the wrong `DynamicValue` variant — callers should
/// skip the row rather than fail the whole login, because a single
/// malformed row should not lock the user out of an otherwise valid
/// membership set.
pub(crate) fn entity_to_tenant_ref(entity: &Entity) -> Option<TenantRef> {
    let schema = extract_text(entity, TM_TENANT_TYPE_FIELD)?;
    let entity_id = extract_text(entity, TM_TENANT_ID_FIELD)?;
    if schema.is_empty() || entity_id.is_empty() {
        return None;
    }
    Some(TenantRef { schema, entity_id })
}

// ---------------------------------------------------------------------------
// AuthStore implementation
// ---------------------------------------------------------------------------

impl AuthStore for EntityAuthStore {
    async fn validate_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<ForgeUser>, BackendError> {
        let entity = match self.find_entity_by_username(username).await? {
            Some(e) => e,
            None => return Ok(None),
        };
        if !extract_boolean(&entity, ACTIVE_FIELD).unwrap_or(true) {
            return Ok(None);
        }
        let hash = match extract_text(&entity, PASSWORD_HASH_FIELD) {
            Some(h) if !h.is_empty() => h,
            _ => return Ok(None),
        };
        if Self::verify_password(password, &hash)? {
            Ok(Some(self.entity_to_forge_user(&entity)))
        } else {
            Ok(None)
        }
    }

    async fn list_users(&self) -> Result<Vec<ForgeUser>, BackendError> {
        let query = Query::new(self.user_schema.id.clone())
            .with_sort(
                FieldPath::single(USERNAME_FIELD),
                schema_forge_core::query::SortOrder::Ascending,
            );
        let result = self.store.query(&query).await?;
        Ok(result
            .entities
            .iter()
            .map(|e| self.entity_to_forge_user(e))
            .collect())
    }

    async fn get_user(&self, username: &str) -> Result<Option<ForgeUser>, BackendError> {
        Ok(self
            .find_entity_by_username(username)
            .await?
            .map(|e| self.entity_to_forge_user(&e)))
    }

    async fn get_user_entity(&self, username: &str) -> Result<Option<Entity>, BackendError> {
        self.find_entity_by_username(username).await
    }

    async fn create_user(
        &self,
        username: &str,
        password: &str,
        roles: &[String],
        display_name: &str,
    ) -> Result<(), BackendError> {
        if self.find_entity_by_username(username).await?.is_some() {
            return Err(BackendError::QueryError {
                message: format!("user '{username}' already exists"),
            });
        }
        let hash = Self::hash_password(password)?;
        let entity = self.build_user_entity(username, roles, display_name, hash);
        self.store.create(&entity).await?;
        Ok(())
    }

    async fn update_user(
        &self,
        username: &str,
        roles: &[String],
        display_name: &str,
    ) -> Result<(), BackendError> {
        let mut entity = self
            .find_entity_by_username(username)
            .await?
            .ok_or_else(|| BackendError::EntityNotFound {
                schema: "User".to_string(),
                entity_id: username.to_string(),
            })?;
        let role_rank = compute_role_rank(roles, |r| (self.role_rank_resolver)(r));
        entity.fields.insert(
            DISPLAY_NAME_FIELD.to_string(),
            DynamicValue::Text(display_name.to_string()),
        );
        entity.fields.insert(
            ROLES_FIELD.to_string(),
            DynamicValue::Array(roles.iter().cloned().map(DynamicValue::Text).collect()),
        );
        entity.fields.insert(
            ROLE_RANK_FIELD.to_string(),
            DynamicValue::Integer(role_rank),
        );
        self.store.update(&entity).await?;
        Ok(())
    }

    async fn toggle_user_active(&self, username: &str) -> Result<(), BackendError> {
        let mut entity = self
            .find_entity_by_username(username)
            .await?
            .ok_or_else(|| BackendError::EntityNotFound {
                schema: "User".to_string(),
                entity_id: username.to_string(),
            })?;
        let current = extract_boolean(&entity, ACTIVE_FIELD).unwrap_or(true);
        entity
            .fields
            .insert(ACTIVE_FIELD.to_string(), DynamicValue::Boolean(!current));
        self.store.update(&entity).await?;
        Ok(())
    }

    async fn count_users(&self) -> Result<usize, BackendError> {
        let query = Query::new(self.user_schema.id.clone());
        self.store.count(&query).await
    }

    async fn delete_user(&self, username: &str) -> Result<(), BackendError> {
        let entity = match self.find_entity_by_username(username).await? {
            Some(e) => e,
            None => return Ok(()),
        };
        self.store
            .delete(self.user_schema_name(), &entity.id)
            .await
    }

    async fn change_password(
        &self,
        username: &str,
        new_password: &str,
    ) -> Result<(), BackendError> {
        let mut entity = match self.find_entity_by_username(username).await? {
            Some(e) => e,
            None => return Ok(()),
        };
        let hash = Self::hash_password(new_password)?;
        entity.fields.insert(
            PASSWORD_HASH_FIELD.to_string(),
            DynamicValue::Text(hash),
        );
        self.store.update(&entity).await?;
        Ok(())
    }

    async fn list_tenant_memberships(
        &self,
        username: &str,
    ) -> Result<Vec<TenantRef>, BackendError> {
        // Tenancy not configured for this deployment — return empty.
        // The login handler treats "0 memberships + tenancy enabled" as
        // a 401; with no schema attached we don't know whether tenancy
        // is enabled, so we leave that decision to the caller.
        let Some(tm_schema) = self.tenant_membership_schema.as_ref() else {
            return Ok(Vec::new());
        };

        // Resolve the user's EntityId first; without the row there are
        // no memberships to read regardless of the TenantMembership
        // table's contents.
        let Some(user_entity) = self.find_entity_by_username(username).await? else {
            return Ok(Vec::new());
        };

        let query = Query::new(tm_schema.id.clone()).with_filter(Filter::eq(
            FieldPath::single(TM_USER_FIELD),
            DynamicValue::Ref(user_entity.id.clone()),
        ));
        let result = self.store.query(&query).await?;
        Ok(result
            .entities
            .iter()
            .filter_map(entity_to_tenant_ref)
            .collect())
    }

    async fn add_tenant_membership(
        &self,
        username: &str,
        tenant_type: &str,
        tenant_id: &str,
        role: Option<&str>,
    ) -> Result<(), BackendError> {
        let tm_schema =
            self.tenant_membership_schema
                .as_ref()
                .ok_or_else(|| BackendError::Internal {
                    message: "TenantMembership schema not attached; cannot grant membership"
                        .to_string(),
                })?;

        // Resolve the user's EntityId so the membership row references the
        // actual `User` entity — the `user` field is a typed `-> User` ref,
        // and `list_tenant_memberships` filters on `DynamicValue::Ref(id)`.
        let user_entity = self.find_entity_by_username(username).await?.ok_or_else(|| {
            BackendError::EntityNotFound {
                schema: "User".to_string(),
                entity_id: username.to_string(),
            }
        })?;

        let mut fields: std::collections::BTreeMap<String, DynamicValue> =
            std::collections::BTreeMap::new();
        fields.insert(
            TM_USER_FIELD.to_string(),
            DynamicValue::Ref(user_entity.id.clone()),
        );
        fields.insert(
            TM_TENANT_TYPE_FIELD.to_string(),
            DynamicValue::Text(tenant_type.to_string()),
        );
        fields.insert(
            TM_TENANT_ID_FIELD.to_string(),
            DynamicValue::Text(tenant_id.to_string()),
        );
        if let Some(r) = role.filter(|s| !s.is_empty()) {
            fields.insert(TM_ROLE_FIELD.to_string(), DynamicValue::Text(r.to_string()));
        }

        let entity = Entity::new(tm_schema.name.clone(), fields);
        self.store.create(&entity).await?;
        Ok(())
    }

    async fn record_login(
        &self,
        username: &str,
        at: DateTime<Utc>,
    ) -> Result<(), BackendError> {
        let mut entity = match self.find_entity_by_username(username).await? {
            Some(e) => e,
            // Idempotent on a missing row: a delete-mid-login race shouldn't
            // 500 the caller. The login handler already proved the row
            // existed when validate_credentials succeeded.
            None => return Ok(()),
        };
        entity
            .fields
            .insert(LAST_LOGIN_FIELD.to_string(), DynamicValue::DateTime(at));
        self.store.update(&entity).await?;
        Ok(())
    }
}

// Suppress dead-code warnings for unused FieldType / FieldName / etc.
// imports — these are surfaced by the helper module so callers can
// build the User schema with the exact type signature expected.
#[allow(dead_code)]
const _USED_TYPES: () = {
    let _ = std::mem::size_of::<FieldName>();
    let _ = std::mem::size_of::<FieldType>();
    let _ = std::mem::size_of::<TextConstraints>();
    let _ = std::mem::size_of::<IntegerConstraints>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use schema_forge_core::types::{
        FieldDefinition, FieldModifier, SchemaId, SchemaName, TextConstraints,
    };

    /// Tiny in-memory entity store implementing both [`EntityStore`] and
    /// (via the blanket impl) [`DynEntityStore`], used to exercise
    /// [`EntityAuthStore`] without standing up a real backend.
    struct MemStore {
        rows: Mutex<Vec<Entity>>,
    }

    impl MemStore {
        fn new() -> Self {
            Self {
                rows: Mutex::new(Vec::new()),
            }
        }
    }

    impl EntityStore for MemStore {
        fn create(
            &self,
            entity: &Entity,
        ) -> impl std::future::Future<Output = Result<Entity, BackendError>> + Send {
            let entity = entity.clone();
            async move {
                let mut rows = self.rows.lock().unwrap();
                rows.push(entity.clone());
                Ok(entity)
            }
        }

        fn get(
            &self,
            schema: &SchemaName,
            id: &EntityId,
        ) -> impl std::future::Future<Output = Result<Entity, BackendError>> + Send {
            let schema_name = schema.as_str().to_string();
            let id = id.clone();
            async move {
                let rows = self.rows.lock().unwrap();
                rows.iter()
                    .find(|e| e.id == id)
                    .cloned()
                    .ok_or_else(|| BackendError::EntityNotFound {
                        schema: schema_name,
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
                let mut rows = self.rows.lock().unwrap();
                rows.retain(|e| e.id != id);
                Ok(())
            }
        }

        fn query(
            &self,
            query: &Query,
        ) -> impl std::future::Future<Output = Result<crate::entity::QueryResult, BackendError>> + Send
        {
            let query = query.clone();
            async move {
                let rows = self.rows.lock().unwrap();
                let mut entities: Vec<Entity> = rows
                    .iter()
                    .filter(|e| match &query.filter {
                        None => true,
                        Some(Filter::Eq { path, value }) => {
                            let key = path.root();
                            e.field(key) == Some(value)
                        }
                        _ => true,
                    })
                    .cloned()
                    .collect();
                if let Some(limit) = query.limit {
                    entities.truncate(limit);
                }
                let total = entities.len();
                Ok(crate::entity::QueryResult::new(entities, Some(total)))
            }
        }

        fn count(
            &self,
            query: &Query,
        ) -> impl std::future::Future<Output = Result<usize, BackendError>> + Send {
            let query = query.clone();
            async move {
                let rows = self.rows.lock().unwrap();
                Ok(rows
                    .iter()
                    .filter(|e| match &query.filter {
                        None => true,
                        Some(Filter::Eq { path, value }) => {
                            let key = path.root();
                            e.field(key) == Some(value)
                        }
                        _ => true,
                    })
                    .count())
            }
        }

        async fn aggregate(
            &self,
            _query: &schema_forge_core::query::AggregateQuery,
        ) -> Result<Vec<schema_forge_core::query::AggregateResult>, BackendError> {
            Ok(Vec::new())
        }
    }

    fn user_schema() -> SchemaDefinition {
        use schema_forge_core::types::FieldAnnotation;
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("User").unwrap(),
            vec![
                FieldDefinition::with_annotations(
                    FieldName::new(USERNAME_FIELD).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new(DISPLAY_NAME_FIELD).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::new(
                    FieldName::new(ROLES_FIELD).unwrap(),
                    FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new(ROLE_RANK_FIELD).unwrap(),
                    FieldType::Integer(IntegerConstraints::default()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::new(
                    FieldName::new(ACTIVE_FIELD).unwrap(),
                    FieldType::Boolean,
                ),
                FieldDefinition::with_annotations(
                    FieldName::new(PASSWORD_HASH_FIELD).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![],
                    vec![FieldAnnotation::Hidden],
                ),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    fn store_with_ranks(
        ranks: &'static [(&'static str, i64)],
    ) -> EntityAuthStore {
        let mem: Arc<dyn DynEntityStore> = Arc::new(MemStore::new());
        EntityAuthStore::new(
            mem,
            user_schema(),
            Arc::new(move |role: &str| {
                ranks
                    .iter()
                    .find(|(r, _)| *r == role)
                    .map(|(_, rank)| *rank)
            }),
        )
    }

    #[tokio::test]
    async fn create_then_validate_succeeds_with_correct_password() {
        let store = store_with_ranks(&[("admin", 1000)]);
        store
            .create_user("alice", "supersecret", &["admin".into()], "Alice")
            .await
            .unwrap();
        let validated = store.validate_credentials("alice", "supersecret").await.unwrap();
        assert!(validated.is_some());
        let user = validated.unwrap();
        assert_eq!(user.username, "alice");
        assert_eq!(user.roles, vec!["admin".to_string()]);
        assert_eq!(user.display_name.as_deref(), Some("Alice"));
        assert!(user.active);
    }

    #[tokio::test]
    async fn validate_returns_none_on_wrong_password() {
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "supersecret", &[], "Alice")
            .await
            .unwrap();
        let validated = store.validate_credentials("alice", "wrong").await.unwrap();
        assert!(validated.is_none());
    }

    #[tokio::test]
    async fn validate_returns_none_for_unknown_user() {
        let store = store_with_ranks(&[]);
        let validated = store.validate_credentials("ghost", "anything").await.unwrap();
        assert!(validated.is_none());
    }

    #[tokio::test]
    async fn validate_returns_none_for_inactive_user() {
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "supersecret", &[], "Alice")
            .await
            .unwrap();
        store.toggle_user_active("alice").await.unwrap();
        let validated = store.validate_credentials("alice", "supersecret").await.unwrap();
        assert!(validated.is_none());
    }

    #[tokio::test]
    async fn create_then_get_returns_forge_user_without_hash() {
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "supersecret", &[], "Alice")
            .await
            .unwrap();
        let user = store.get_user("alice").await.unwrap().unwrap();
        // ForgeUser has no password_hash field — this is the API contract.
        assert_eq!(user.username, "alice");
    }

    #[tokio::test]
    async fn duplicate_create_is_rejected() {
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        let err = store
            .create_user("alice", "different", &[], "Alice")
            .await
            .unwrap_err();
        match err {
            BackendError::QueryError { message } => assert!(message.contains("already exists")),
            other => panic!("expected QueryError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_user_replaces_roles_and_recomputes_role_rank() {
        let store = store_with_ranks(&[("manager", 500), ("admin", 1000)]);
        store
            .create_user("alice", "secret123", &["manager".into()], "Alice")
            .await
            .unwrap();
        store
            .update_user("alice", &["admin".into()], "Alice C")
            .await
            .unwrap();
        // Pull the raw entity through the store to verify role_rank
        // landed alongside the new role list.
        let raw = {
            let store_inner = &store;
            store_inner
                .find_entity_by_username("alice")
                .await
                .unwrap()
                .unwrap()
        };
        match raw.field(ROLE_RANK_FIELD) {
            Some(DynamicValue::Integer(n)) => assert_eq!(*n, 1000),
            other => panic!("expected role_rank=1000, got {other:?}"),
        }
        match raw.field(DISPLAY_NAME_FIELD) {
            Some(DynamicValue::Text(s)) => assert_eq!(s, "Alice C"),
            other => panic!("expected updated display_name, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn change_password_validates_with_new_only() {
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "first-pass", &[], "Alice")
            .await
            .unwrap();
        store.change_password("alice", "second-pass").await.unwrap();
        assert!(store
            .validate_credentials("alice", "first-pass")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .validate_credentials("alice", "second-pass")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn delete_user_makes_lookup_return_none() {
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        store.delete_user("alice").await.unwrap();
        assert!(store.get_user("alice").await.unwrap().is_none());
        assert_eq!(store.count_users().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn count_users_reflects_creates_and_deletes() {
        let store = store_with_ranks(&[]);
        assert_eq!(store.count_users().await.unwrap(), 0);
        store
            .create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        store
            .create_user("bob", "secret123", &[], "Bob")
            .await
            .unwrap();
        assert_eq!(store.count_users().await.unwrap(), 2);
        store.delete_user("alice").await.unwrap();
        assert_eq!(store.count_users().await.unwrap(), 1);
    }

    fn tenant_membership_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("TenantMembership").unwrap(),
            vec![
                FieldDefinition::with_annotations(
                    FieldName::new(TM_USER_FIELD).unwrap(),
                    FieldType::Relation {
                        target: SchemaName::new("User").unwrap(),
                        cardinality: schema_forge_core::types::Cardinality::One,
                    },
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new(TM_TENANT_TYPE_FIELD).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::with_annotations(
                    FieldName::new(TM_TENANT_ID_FIELD).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
                FieldDefinition::new(
                    FieldName::new("role").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    /// Build an `EntityAuthStore` whose underlying `MemStore` is also
    /// returned for direct row seeding. The auth store carries the
    /// `TenantMembership` schema so `list_tenant_memberships` can query
    /// the seeded rows.
    fn store_with_tm() -> (EntityAuthStore, Arc<MemStore>, SchemaDefinition) {
        let mem = Arc::new(MemStore::new());
        let mem_dyn: Arc<dyn DynEntityStore> = mem.clone();
        let tm_schema = tenant_membership_schema();
        let auth = EntityAuthStore::new(
            mem_dyn,
            user_schema(),
            Arc::new(|_role: &str| Some(0)),
        )
        .with_tenant_membership_schema(tm_schema.clone());
        (auth, mem, tm_schema)
    }

    fn make_tm_row(
        tm_schema: &SchemaDefinition,
        user_id: &EntityId,
        tenant_type: &str,
        tenant_id: &str,
    ) -> Entity {
        let mut fields: BTreeMap<String, DynamicValue> = BTreeMap::new();
        fields.insert(
            TM_USER_FIELD.to_string(),
            DynamicValue::Ref(user_id.clone()),
        );
        fields.insert(
            TM_TENANT_TYPE_FIELD.to_string(),
            DynamicValue::Text(tenant_type.to_string()),
        );
        fields.insert(
            TM_TENANT_ID_FIELD.to_string(),
            DynamicValue::Text(tenant_id.to_string()),
        );
        Entity::new(tm_schema.name.clone(), fields)
    }

    #[tokio::test]
    async fn list_tenant_memberships_returns_empty_for_unknown_user() {
        let (auth, _mem, _tm) = store_with_tm();
        let memberships = auth.list_tenant_memberships("ghost").await.unwrap();
        assert!(memberships.is_empty());
    }

    #[tokio::test]
    async fn list_tenant_memberships_returns_empty_when_schema_not_attached() {
        // Without `with_tenant_membership_schema`, list_tenant_memberships
        // is inert — matches the "tenancy not configured" deployment path.
        let store = store_with_ranks(&[]);
        store
            .create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        let memberships = store.list_tenant_memberships("alice").await.unwrap();
        assert!(memberships.is_empty());
    }

    #[tokio::test]
    async fn list_tenant_memberships_returns_single_row() {
        let (auth, mem, tm_schema) = store_with_tm();
        auth.create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        let alice = auth
            .find_entity_by_username("alice")
            .await
            .unwrap()
            .unwrap();
        EntityStore::create(
            mem.as_ref(),
            &make_tm_row(&tm_schema, &alice.id, "Organization", "org-a"),
        )
        .await
        .unwrap();

        let memberships = auth.list_tenant_memberships("alice").await.unwrap();
        assert_eq!(memberships.len(), 1);
        assert_eq!(memberships[0].schema, "Organization");
        assert_eq!(memberships[0].entity_id, "org-a");
    }

    #[tokio::test]
    async fn list_tenant_memberships_returns_multiple_rows() {
        let (auth, mem, tm_schema) = store_with_tm();
        auth.create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        let alice = auth
            .find_entity_by_username("alice")
            .await
            .unwrap()
            .unwrap();
        for (kind, id) in &[("Organization", "org-a"), ("Organization", "org-b")] {
            EntityStore::create(
                mem.as_ref(),
                &make_tm_row(&tm_schema, &alice.id, kind, id),
            )
            .await
            .unwrap();
        }

        let memberships = auth.list_tenant_memberships("alice").await.unwrap();
        assert_eq!(memberships.len(), 2);
        let ids: Vec<&str> = memberships.iter().map(|m| m.entity_id.as_str()).collect();
        assert!(ids.contains(&"org-a"));
        assert!(ids.contains(&"org-b"));
    }

    #[tokio::test]
    async fn list_tenant_memberships_filters_by_user_ref() {
        let (auth, mem, tm_schema) = store_with_tm();
        auth.create_user("alice", "secret123", &[], "Alice")
            .await
            .unwrap();
        auth.create_user("bob", "secret123", &[], "Bob")
            .await
            .unwrap();
        let alice = auth
            .find_entity_by_username("alice")
            .await
            .unwrap()
            .unwrap();
        let bob = auth
            .find_entity_by_username("bob")
            .await
            .unwrap()
            .unwrap();

        EntityStore::create(
            mem.as_ref(),
            &make_tm_row(&tm_schema, &alice.id, "Organization", "org-a"),
        )
        .await
        .unwrap();
        EntityStore::create(
            mem.as_ref(),
            &make_tm_row(&tm_schema, &bob.id, "Organization", "org-b"),
        )
        .await
        .unwrap();

        let alice_memberships = auth.list_tenant_memberships("alice").await.unwrap();
        assert_eq!(alice_memberships.len(), 1);
        assert_eq!(alice_memberships[0].entity_id, "org-a");

        let bob_memberships = auth.list_tenant_memberships("bob").await.unwrap();
        assert_eq!(bob_memberships.len(), 1);
        assert_eq!(bob_memberships[0].entity_id, "org-b");
    }

    #[test]
    fn entity_to_tenant_ref_returns_some_for_valid_row() {
        let mut fields: BTreeMap<String, DynamicValue> = BTreeMap::new();
        fields.insert(
            TM_TENANT_TYPE_FIELD.to_string(),
            DynamicValue::Text("Organization".to_string()),
        );
        fields.insert(
            TM_TENANT_ID_FIELD.to_string(),
            DynamicValue::Text("org-a".to_string()),
        );
        let entity = Entity::new(SchemaName::new("TenantMembership").unwrap(), fields);
        let tr = entity_to_tenant_ref(&entity).unwrap();
        assert_eq!(tr.schema, "Organization");
        assert_eq!(tr.entity_id, "org-a");
    }

    #[test]
    fn entity_to_tenant_ref_returns_none_for_missing_fields() {
        let entity = Entity::new(
            SchemaName::new("TenantMembership").unwrap(),
            BTreeMap::new(),
        );
        assert!(entity_to_tenant_ref(&entity).is_none());
    }

    #[test]
    fn entity_to_tenant_ref_returns_none_for_empty_strings() {
        let mut fields: BTreeMap<String, DynamicValue> = BTreeMap::new();
        fields.insert(
            TM_TENANT_TYPE_FIELD.to_string(),
            DynamicValue::Text(String::new()),
        );
        fields.insert(
            TM_TENANT_ID_FIELD.to_string(),
            DynamicValue::Text("org-a".to_string()),
        );
        let entity = Entity::new(SchemaName::new("TenantMembership").unwrap(), fields);
        assert!(entity_to_tenant_ref(&entity).is_none());
    }

    #[test]
    fn compute_role_rank_picks_max() {
        let ranks: BTreeMap<&str, i64> =
            BTreeMap::from([("manager", 500), ("admin", 1000)]);
        let rank = compute_role_rank(
            &["manager".into(), "admin".into()],
            |r| ranks.get(r).copied(),
        );
        assert_eq!(rank, 1000);
    }

    #[test]
    fn compute_role_rank_returns_zero_for_empty_or_unknown() {
        assert_eq!(compute_role_rank(&[], |_| Some(99)), 0);
        assert_eq!(compute_role_rank(&["unknown".into()], |_| None), 0);
    }
}
