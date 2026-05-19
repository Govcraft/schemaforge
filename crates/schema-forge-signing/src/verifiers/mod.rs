//! Concrete verifier implementations.
//!
//! Each submodule implements [`crate::verifier::SchemaVerifier`] for one
//! signing scheme. Code outside this module should depend on the trait,
//! not on the concrete types — the trait is the contract the policy
//! engine binds against.

pub mod ed25519;
