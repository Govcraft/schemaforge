//! Pending-invitation store for the user invite/onboard flow (issue #71).
//!
//! # Why a store, not a public schema
//!
//! An invitation row carries security-sensitive material — the full minted
//! PASETO invite token — and must never be reachable through the generic
//! entity CRUD API the way `User` or `TenantMembership` are. SchemaForge's
//! backends expose no storage primitive *other* than the schema/entity
//! machinery, so this store reuses that machinery against an internal
//! `ForgeInvitation` table while keeping the boundary at the registry: the
//! provisioning step (in the acton layer) creates the table and persists its
//! metadata for idempotent reboots, but **never inserts it into the in-memory
//! `SchemaRegistry`**. Because `/schemas` and every entity route resolve
//! schemas through that registry, the table is invisible to the public API —
//! a store in every externally observable sense.
//!
//! # Single-use, expiring tokens
//!
//! The emailed accept link carries only the invitation's `jti` (a
//! high-entropy token id minted into the PASETO). On accept, the handler
//! looks the row up by `jti`, *reconstructs the full PASETO from `token`*,
//! verifies it cryptographically, and then checks [`InviteStatus`] and
//! [`ForgeInvitation::is_expired`] before consuming it. Consumption flips the
//! status to [`InviteStatus::Consumed`] so a replayed link is rejected.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use schema_forge_core::query::{Filter, FieldPath, Query};
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition, SchemaName};
use serde::{Deserialize, Serialize};

use crate::entity::Entity;
use crate::entity_auth_store::DynEntityStore;
use crate::error::BackendError;

/// Internal schema backing the invitation store.
///
/// Parsed and provisioned by the acton layer; intentionally absent from the
/// public schema registry (see the module docs). Timestamps are stored as
/// RFC3339 `text` rather than `datetime` so the store stays fully
/// backend-agnostic and owns its own expiry comparison.
pub const FORGE_INVITATION_SCHEMA: &str = r#"
@system
schema ForgeInvitation {
    email:        text(max: 512) required indexed
    display_name: text(max: 255)
    tenant_type:  text(max: 128)
    tenant_id:    text(max: 255)
    role:         text(max: 128)
    jti:          text(max: 128) required indexed
    token:        text(max: 4096) required
    status:       text(max: 32) required indexed
    expires_at:   text(max: 64)
    invited_by:   text(max: 255)
    consumed_at:  text(max: 64)
}
"#;

const F_EMAIL: &str = "email";
const F_DISPLAY_NAME: &str = "display_name";
const F_TENANT_TYPE: &str = "tenant_type";
const F_TENANT_ID: &str = "tenant_id";
const F_ROLE: &str = "role";
const F_JTI: &str = "jti";
const F_TOKEN: &str = "token";
const F_STATUS: &str = "status";
const F_EXPIRES_AT: &str = "expires_at";
const F_INVITED_BY: &str = "invited_by";
const F_CONSUMED_AT: &str = "consumed_at";

/// Lifecycle state of an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteStatus {
    /// Issued and awaiting acceptance.
    Pending,
    /// Accepted; the link is spent and must not be reusable.
    Consumed,
    /// Explicitly revoked by an operator before acceptance.
    Revoked,
}

impl InviteStatus {
    /// Wire/storage string for the `status` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Consumed => "consumed",
            Self::Revoked => "revoked",
        }
    }

    /// Parse a stored `status` string, returning `None` for unknown values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "consumed" => Some(Self::Consumed),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// Fields needed to issue a new invitation. The caller is responsible for
/// minting `jti`/`token` (the PASETO) and choosing `expires_at`.
#[derive(Debug, Clone)]
pub struct NewInvitation {
    /// Invitee email (the future `User.email`).
    pub email: String,
    /// Optional display name to seed onto the pending user.
    pub display_name: Option<String>,
    /// Tenant root type the invitee is being added to (e.g. `"Organization"`).
    pub tenant_type: Option<String>,
    /// Tenant root entity id.
    pub tenant_id: Option<String>,
    /// Role granted within that tenant.
    pub role: Option<String>,
    /// Token id minted into the PASETO; the opaque value emailed to the user.
    pub jti: String,
    /// The full minted PASETO invite token, reconstructed and verified on accept.
    pub token: String,
    /// Absolute expiry.
    pub expires_at: DateTime<Utc>,
    /// `sub` of the operator who issued the invite, for audit.
    pub invited_by: Option<String>,
}

/// A persisted invitation row.
#[derive(Debug, Clone)]
pub struct ForgeInvitation {
    /// Entity id of the row (used to consume it).
    pub id: EntityId,
    pub email: String,
    pub display_name: Option<String>,
    pub tenant_type: Option<String>,
    pub tenant_id: Option<String>,
    pub role: Option<String>,
    pub jti: String,
    pub token: String,
    pub status: InviteStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub invited_by: Option<String>,
    pub consumed_at: Option<DateTime<Utc>>,
}

impl ForgeInvitation {
    /// True if the invitation has passed its expiry as of `now`.
    ///
    /// A row with an unparseable/absent `expires_at` is treated as expired —
    /// fail closed rather than honour an invite with no enforceable deadline.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            Some(exp) => now >= exp,
            None => true,
        }
    }

    /// True if the invitation can still be accepted at `now`.
    pub fn is_acceptable(&self, now: DateTime<Utc>) -> bool {
        self.status == InviteStatus::Pending && !self.is_expired(now)
    }
}

/// Storage-agnostic operations over the pending-invitation table.
#[async_trait::async_trait]
pub trait InviteStore: Send + Sync {
    /// Persist a new pending invitation.
    async fn create(&self, invite: NewInvitation) -> Result<ForgeInvitation, BackendError>;

    /// Look an invitation up by its `jti` (the emailed opaque reference).
    async fn find_by_jti(&self, jti: &str) -> Result<Option<ForgeInvitation>, BackendError>;

    /// Mark an invitation consumed at `at`. Idempotent at the storage layer;
    /// callers enforce single-use by checking [`ForgeInvitation::is_acceptable`]
    /// before calling.
    async fn mark_consumed(&self, id: &EntityId, at: DateTime<Utc>)
        -> Result<(), BackendError>;
}

/// [`InviteStore`] backed by the internal `ForgeInvitation` entity table.
///
/// Mirrors `EntityAuthStore`: holds a type-erased entity store and the
/// resolved `ForgeInvitation` [`SchemaDefinition`] so it can address the
/// table by `SchemaId` without consulting any registry.
pub struct EntityInviteStore {
    store: Arc<dyn DynEntityStore>,
    schema: SchemaDefinition,
}

impl EntityInviteStore {
    /// Construct the store from the entity-store handle and the parsed
    /// `ForgeInvitation` schema definition.
    pub fn new(store: Arc<dyn DynEntityStore>, schema: SchemaDefinition) -> Self {
        debug_assert_eq!(
            schema.name.as_str(),
            "ForgeInvitation",
            "EntityInviteStore must be constructed with the ForgeInvitation SchemaDefinition"
        );
        Self { store, schema }
    }

    fn schema_name(&self) -> &SchemaName {
        &self.schema.name
    }
}

#[async_trait::async_trait]
impl InviteStore for EntityInviteStore {
    async fn create(&self, invite: NewInvitation) -> Result<ForgeInvitation, BackendError> {
        let entity = build_invitation_entity(self.schema_name(), &invite);
        let created = self.store.create(&entity).await?;
        entity_to_invitation(&created).ok_or_else(|| BackendError::Internal {
            message: "ForgeInvitation row malformed on readback after create".to_string(),
        })
    }

    async fn find_by_jti(&self, jti: &str) -> Result<Option<ForgeInvitation>, BackendError> {
        let query = Query::new(self.schema.id.clone())
            .with_filter(Filter::eq(
                FieldPath::single(F_JTI),
                DynamicValue::Text(jti.to_string()),
            ))
            .with_limit(1);
        let result = self.store.query(&query).await?;
        Ok(result.entities.iter().find_map(entity_to_invitation))
    }

    async fn mark_consumed(
        &self,
        id: &EntityId,
        at: DateTime<Utc>,
    ) -> Result<(), BackendError> {
        let mut entity = self.store.get(self.schema_name(), id).await?;
        entity.fields.insert(
            F_STATUS.to_string(),
            DynamicValue::Text(InviteStatus::Consumed.as_str().to_string()),
        );
        entity.fields.insert(
            F_CONSUMED_AT.to_string(),
            DynamicValue::Text(at.to_rfc3339()),
        );
        self.store.update(&entity).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pure entity <-> record mappers (unit-tested)
// ---------------------------------------------------------------------------

fn text_field(value: Option<String>) -> Option<DynamicValue> {
    value
        .filter(|s| !s.is_empty())
        .map(DynamicValue::Text)
}

fn build_invitation_entity(schema: &SchemaName, invite: &NewInvitation) -> Entity {
    let mut fields: std::collections::BTreeMap<String, DynamicValue> =
        std::collections::BTreeMap::new();
    fields.insert(F_EMAIL.to_string(), DynamicValue::Text(invite.email.clone()));
    if let Some(v) = text_field(invite.display_name.clone()) {
        fields.insert(F_DISPLAY_NAME.to_string(), v);
    }
    if let Some(v) = text_field(invite.tenant_type.clone()) {
        fields.insert(F_TENANT_TYPE.to_string(), v);
    }
    if let Some(v) = text_field(invite.tenant_id.clone()) {
        fields.insert(F_TENANT_ID.to_string(), v);
    }
    if let Some(v) = text_field(invite.role.clone()) {
        fields.insert(F_ROLE.to_string(), v);
    }
    fields.insert(F_JTI.to_string(), DynamicValue::Text(invite.jti.clone()));
    fields.insert(F_TOKEN.to_string(), DynamicValue::Text(invite.token.clone()));
    fields.insert(
        F_STATUS.to_string(),
        DynamicValue::Text(InviteStatus::Pending.as_str().to_string()),
    );
    fields.insert(
        F_EXPIRES_AT.to_string(),
        DynamicValue::Text(invite.expires_at.to_rfc3339()),
    );
    if let Some(v) = text_field(invite.invited_by.clone()) {
        fields.insert(F_INVITED_BY.to_string(), v);
    }
    Entity::new(schema.clone(), fields)
}

fn extract_text(entity: &Entity, field: &str) -> Option<String> {
    match entity.field(field) {
        Some(DynamicValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn extract_nonempty(entity: &Entity, field: &str) -> Option<String> {
    extract_text(entity, field).filter(|s| !s.is_empty())
}

fn parse_ts(entity: &Entity, field: &str) -> Option<DateTime<Utc>> {
    let raw = extract_nonempty(entity, field)?;
    DateTime::parse_from_rfc3339(&raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Build a [`ForgeInvitation`] from a stored row. Returns `None` if a required
/// column (`id`, `email`, `jti`, `token`, `status`) is missing or malformed —
/// a half-written row is unusable and must not masquerade as a valid invite.
fn entity_to_invitation(entity: &Entity) -> Option<ForgeInvitation> {
    let id = entity.id.clone();
    let email = extract_nonempty(entity, F_EMAIL)?;
    let jti = extract_nonempty(entity, F_JTI)?;
    let token = extract_nonempty(entity, F_TOKEN)?;
    let status = InviteStatus::parse(&extract_text(entity, F_STATUS)?)?;
    Some(ForgeInvitation {
        id,
        email,
        display_name: extract_nonempty(entity, F_DISPLAY_NAME),
        tenant_type: extract_nonempty(entity, F_TENANT_TYPE),
        tenant_id: extract_nonempty(entity, F_TENANT_ID),
        role: extract_nonempty(entity, F_ROLE),
        jti,
        token,
        status,
        expires_at: parse_ts(entity, F_EXPIRES_AT),
        invited_by: extract_nonempty(entity, F_INVITED_BY),
        consumed_at: parse_ts(entity, F_CONSUMED_AT),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn sample_new() -> NewInvitation {
        NewInvitation {
            email: "invitee@example.gov".to_string(),
            display_name: Some("Pat Invitee".to_string()),
            tenant_type: Some("Organization".to_string()),
            tenant_id: Some("org_123".to_string()),
            role: Some("member".to_string()),
            jti: "jti-abc".to_string(),
            token: "v4.local.deadbeef".to_string(),
            expires_at: Utc::now() + Duration::days(7),
            invited_by: Some("user:admin".to_string()),
        }
    }

    #[test]
    fn status_roundtrips() {
        for s in [InviteStatus::Pending, InviteStatus::Consumed, InviteStatus::Revoked] {
            assert_eq!(InviteStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(InviteStatus::parse("bogus"), None);
    }

    #[test]
    fn build_then_read_roundtrips_core_fields() {
        let schema = SchemaName::new("ForgeInvitation").unwrap();
        let invite = sample_new();
        // `Entity::new` assigns the id the backend would persist.
        let entity = build_invitation_entity(&schema, &invite);
        let rec = entity_to_invitation(&entity).expect("row should parse");
        assert_eq!(rec.email, "invitee@example.gov");
        assert_eq!(rec.jti, "jti-abc");
        assert_eq!(rec.token, "v4.local.deadbeef");
        assert_eq!(rec.status, InviteStatus::Pending);
        assert_eq!(rec.role.as_deref(), Some("member"));
        assert_eq!(rec.tenant_type.as_deref(), Some("Organization"));
        assert!(rec.expires_at.is_some());
        assert!(rec.consumed_at.is_none());
    }

    #[test]
    fn missing_required_field_yields_none() {
        let schema = SchemaName::new("ForgeInvitation").unwrap();
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(F_EMAIL.to_string(), DynamicValue::Text("a@b.gov".to_string()));
        // No jti/token/status.
        let entity = Entity::new(schema.clone(), fields);
        assert!(entity_to_invitation(&entity).is_none());
    }

    #[test]
    fn expiry_and_acceptability() {
        let schema = SchemaName::new("ForgeInvitation").unwrap();
        let mut invite = sample_new();
        invite.expires_at = Utc::now() - Duration::hours(1);
        let entity = build_invitation_entity(&schema, &invite);
        let rec = entity_to_invitation(&entity).unwrap();
        let now = Utc::now();
        assert!(rec.is_expired(now));
        assert!(!rec.is_acceptable(now));
    }

    #[test]
    fn absent_expiry_is_treated_as_expired() {
        let rec = ForgeInvitation {
            id: EntityId::new("ForgeInvitation"),
            email: "a@b.gov".to_string(),
            display_name: None,
            tenant_type: None,
            tenant_id: None,
            role: None,
            jti: "j".to_string(),
            token: "t".to_string(),
            status: InviteStatus::Pending,
            expires_at: None,
            invited_by: None,
            consumed_at: None,
        };
        assert!(rec.is_expired(Utc::now()));
    }
}
