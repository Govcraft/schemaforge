//! Verifier trait and verified-identity types.
//!
//! `SchemaVerifier` is the common boundary every signature backend
//! implements. The high-level [`crate::policy::VerifyPolicy`] holds a
//! list of these and accepts a file if any verifier in the list
//! verifies it (OR-semantics — additive rotation).

use std::path::Path;

use crate::error::VerifyError;

/// What a verifier returns when a signature is accepted. Carried into
/// audit logs so an operator can replay "who signed schema X."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    /// Stable string discriminating the verifier kind (`"ed25519"`,
    /// `"ssh"`, `"cosign-keyless"`, …). Useful for telemetry that
    /// wants to bucket by signer scheme without keeping a list of
    /// scheme-typed enum variants in sync.
    pub kind: &'static str,

    /// Human-readable name from the trust policy (e.g., `"ops-key"`
    /// for an ed25519 trust anchor, or `"github-actions:GovCraft/..."`
    /// for a cosign workload identity).
    pub name: String,
}

/// One signing scheme.
///
/// Implementations must be deterministic and side-effect-free given
/// the same inputs — a verification call is a pure function from
/// `(bytes, sig, cert)` to `Result<VerifiedIdentity, VerifyError>`.
/// Network IO is permitted only when explicitly opted into by the
/// trust-policy variant (e.g., cosign-keyless online mode).
pub trait SchemaVerifier: Send + Sync {
    /// Stable name used in error reasons and audit lines.
    fn name(&self) -> &str;

    /// Stable kind string (matches [`VerifiedIdentity::kind`]).
    fn kind(&self) -> &'static str;

    /// Verify that `signature` is a valid signature over `file_bytes`
    /// according to this trust anchor.
    ///
    /// `cert` is the optional certificate sidecar produced by cosign
    /// keyless. Verifiers that don't need it ignore it.
    fn verify(
        &self,
        file_path: &Path,
        file_bytes: &[u8],
        signature: &[u8],
        cert: Option<&[u8]>,
    ) -> Result<VerifiedIdentity, VerifyError>;
}
