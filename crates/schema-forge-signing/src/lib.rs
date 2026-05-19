//! Digital-signature verification for SchemaForge `.schema` files.
//!
//! Two threats this crate defends against:
//!
//! 1. **On-disk tampering** — anyone with filesystem write access to
//!    `schemas/` can otherwise introduce or modify entity
//!    definitions, Cedar policies, and `@hook(...)` annotations that
//!    subsequently get applied to the database. Per-file detached
//!    signatures bind every byte of every `.schema` to a cryptographic
//!    identity.
//! 2. **Untrusted-author provenance** — even with the file intact,
//!    there must be a verifiable binding between schemas and an
//!    authorized human or workload identity. The trust policy
//!    enumerates *which* identities are permitted to sign; missing
//!    that match, the signature is still rejected.
//!
//! ## File layout
//!
//! ```text
//! schemas/
//!   user.schema
//!   user.schema.sig            # detached signature (base64)
//!   user.schema.pem            # cosign Fulcio cert (keyless only)
//!   schemas.manifest.toml      # enumerates expected files + sha256
//!   schemas.manifest.toml.sig
//!   schemas.manifest.toml.pem
//! ```
//!
//! ## Verification flow
//!
//! 1. Read and verify the manifest signature against the trust policy.
//! 2. Cross-check: disk contents == manifest entries (no extras, no
//!    missing). Defeats add/remove attacks the per-file scheme can't
//!    see alone.
//! 3. For each file: verify the per-file `.sig` and check the SHA-256
//!    matches the manifest's pinned hash.
//!
//! ## Modes
//!
//! `signing.mode` selects behavior on failure:
//!
//! - `off` — skip verification entirely; emit one WARN at startup.
//! - `warn` — run all checks; log failures, do not abort.
//! - `enforce` — run all checks; any failure aborts the command.
//!
//! ## Adding new verifier backends
//!
//! Implement [`verifier::SchemaVerifier`] for the new scheme, add a
//! variant to [`config::TrustedSigner`], and dispatch the variant in
//! [`policy::VerifyPolicy::from_config`]. Trust policy is purely
//! additive — an old config keeps working when new variants land.

pub mod config;
mod error;
mod hash;
pub mod manifest;
pub mod policy;
pub mod signer;
pub mod verifier;
pub mod verifiers;

pub use config::{SigningConfig, SigningMode, TrustedSigner};
pub use error::{SigningError, VerifyError};
pub use manifest::{Manifest, ManifestEntry, MANIFEST_FILE_NAME, MANIFEST_SCHEMA_VERSION};
pub use policy::{FileVerifyOutcome, FileVerifyResult, VerifyPolicy, VerifyReport};
pub use signer::{sign_directory, DirectorySigner, Ed25519Signer, SignReport, SignedFile, SshSigner};
pub use verifier::{SchemaVerifier, VerifiedIdentity};
pub use verifiers::ed25519::{signature_path_for, Ed25519Verifier};
pub use verifiers::ssh::{SshAllowedSignersVerifier, SSHSIG_NAMESPACE};
