//! Cedar policy and schema source generation.
//!
//! Pure functions translating SchemaForge schema definitions into Cedar
//! source text. Output of [`policy_gen::generate_cedar_policies`] and
//! [`schema_gen::generate_cedar_schema`] is fed to
//! `crate::authz::store::PolicyStoreSnapshot::compile`, which validates
//! the bundle and produces a runtime-ready
//! [`crate::authz::PolicyStoreSnapshot`].

pub mod policy_gen;
pub mod schema_gen;

pub use policy_gen::{generate_cedar_policies, CedarPolicy};
pub use schema_gen::{generate_cedar_schema, SchemaGenError};
