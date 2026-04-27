//! Record-level access control trait.
//!
//! `schema-forge-backend` exposes the abstraction; the Cedar-backed default
//! implementation lives in `schema-forge-acton::authz::CedarRecordPolicy`,
//! where the policy store and authorization engine are available. Operators
//! who want to plug in a custom record policy implement this trait and
//! register it via `SchemaForgeExtension::with_record_access_policy`.

use std::future::Future;
use std::pin::Pin;

use acton_service::middleware::Claims;
use schema_forge_core::types::SchemaDefinition;

use crate::entity::Entity;

/// The dedicated platform-superuser role string.
///
/// Bypasses every server-side authorization check and gates the
/// platform-management endpoints. Distinct from any in-app role string an
/// application may choose, so applications are free to use `admin` or any
/// other label for their highest in-app tier without inheriting platform
/// privileges.
///
/// Cedar policies confer this bypass via the global
/// `forge.global.platform_admin_permit` rule emitted by the policy
/// generator; this constant remains as the canonical role-name string for
/// JWT issuance and bootstrap routines.
pub const PLATFORM_ADMIN_ROLE: &str = "platform_admin";

/// Trait for record-level access control.
///
/// Implementations decide whether the authenticated user can see, modify, or
/// delete individual records. The default implementation supplied by
/// `schema-forge-acton` evaluates each call through the Cedar policy
/// engine; alternative implementations can replace it via
/// `SchemaForgeExtension::with_record_access_policy`.
pub trait RecordAccessPolicy: Send + Sync {
    /// Filter a list of entities to only those visible to the authenticated user.
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>>;

    /// Check if the user can modify (update) the given entity.
    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// Check if the user can delete the given entity.
    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}
