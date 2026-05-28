//! Cross-cutting HTTP middleware for the JSON forge API.
//!
//! Each sub-module exposes a single `axum::middleware::from_fn`-style
//! middleware plus any state struct it needs. Middleware is layered onto
//! the versioned router in `schema_forge_cli::commands::serve`.

pub mod tenant_scope;
