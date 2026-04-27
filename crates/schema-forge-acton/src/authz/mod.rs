//! Cedar-backed authorization engine.
//!
//! SchemaForge's authorization story is owned end-to-end by the Cedar policy
//! engine. The DSL `@access`, `@field_access`, `@owner`, and `@tenant`
//! annotations drive policy generation, the generated policies plus any
//! operator-supplied custom policies are validated against a Cedar schema
//! at compile time, and every authorization decision the server makes
//! evaluates that compiled policy set. Hand-rolled access checks have been
//! retired; this module is the only path through which authorization
//! decisions can flow.
//!
//! ## Module layout
//!
//! - [`store`] — the in-process compiled-policy cache backed by `ArcSwap`,
//!   atomically swapped when schemas or custom policies change.
//! - [`adapters`] — pure functions translating SchemaForge domain types
//!   ([`Claims`], [`SchemaDefinition`], [`Entity`]) into Cedar entities,
//!   actions, and requests.
//! - [`engine`] — [`authorize`] and the field-level [`authorize_field`]
//!   entry points. Both run audit logging on every decision.
//! - [`loader`] — disk loader for `policies/custom/*.cedar` and
//!   `policies/role_ranks.toml`.
//! - [`namespace`] — string constants for the `Forge::` Cedar namespace
//!   under which SchemaForge owns its built-in entity types and actions.
//!
//! [`Claims`]: acton_service::middleware::Claims
//! [`SchemaDefinition`]: schema_forge_core::types::SchemaDefinition
//! [`Entity`]: schema_forge_backend::entity::Entity

pub mod adapters;
pub mod engine;
pub mod loader;
pub mod namespace;
pub mod role_ranks;
pub mod store;

pub use engine::{authorize, authorize_field, AuthzDecision, AuthzError, FieldDirection};
pub use loader::{load_custom_policies, CustomPolicySource};
pub use role_ranks::{RoleRank, RoleRanks, PLATFORM_ADMIN_RANK};
pub use store::{PolicyStore, PolicyStoreSnapshot};
