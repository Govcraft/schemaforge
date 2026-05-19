//! Cosign-keyless verifier.
//!
//! Accepts the modern Sigstore Bundle format (`mediaType =
//! application/vnd.dev.sigstore.bundle.v0.3+json`), what `cosign
//! sign-blob --bundle <out>` writes today. The bundle carries the
//! short-lived Fulcio certificate, the detached signature, the Rekor
//! transparency-log entry, and an optional RFC3161 timestamp — every
//! piece needed for offline verification once a Sigstore trust root is
//! loaded.
//!
//! ## Why bundles and not `.sig + .pem`
//!
//! Legacy `cosign sign-blob` output (before `--bundle`) writes a
//! detached `.sig` and a `.pem` cert. That format does NOT include the
//! Rekor inclusion proof, so an offline verifier cannot prove that the
//! Fulcio cert was valid at sign time — its validity window is ~10
//! minutes, long expired by deployment time. Bundle format embeds the
//! Rekor entry whose `integratedTime` becomes the trusted historical
//! signing time, so verification works long after the cert expired.
//! Bundle format is also where the Sigstore ecosystem (cosign v3+,
//! sigstore-python, sigstore-go, GitHub artifact attestations) is
//! converging, so picking it means SchemaForge `.sig` files round-trip
//! cleanly with the rest of the world.
//!
//! ## What gets verified
//!
//! In order, per [`sigstore_verify`]'s implementation:
//!
//! 1. The Fulcio cert chains to the Sigstore root (loaded from the
//!    bundled production trust root unless the operator supplied a
//!    `trust_root_bundle` override — Phase 4).
//! 2. The cert's Signed Certificate Timestamp (SCT) is valid.
//! 3. The cert's OIDC issuer (extension OID 1.3.6.1.4.1.57264.1.1)
//!    equals the policy's `issuer` exactly.
//! 4. The Rekor transparency-log entry's inclusion proof verifies
//!    against the trusted root's Rekor key, AND the `integratedTime`
//!    falls inside the cert's `notBefore`/`notAfter` window.
//! 5. The detached signature verifies against the cert's public key
//!    over the artifact bytes.
//! 6. Layered on top of (3): the cert's SubjectAlternativeName URI or
//!    email (the OIDC subject — what GitHub Actions writes as
//!    `https://github.com/OWNER/REPO/...@refs/...`) is matched against
//!    the policy's `subject_pattern` as a glob.
//!
//! Step (6) is the SchemaForge-specific layer; `sigstore-verify`
//! exposes the verified identity but only does exact-string match
//! itself.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use sigstore_trust_root::{TrustedRoot, SIGSTORE_PRODUCTION_TRUSTED_ROOT};
use sigstore_verify::types::Bundle;
use sigstore_verify::{VerificationPolicy, Verifier as SigstoreVerifier};

use crate::error::{SigningError, VerifyError};
use crate::verifier::{SchemaVerifier, VerifiedIdentity};

/// Cosign-keyless verifier built against the Sigstore production trust
/// root.
///
/// `name` is the operator-friendly label from the trust policy.
/// `issuer` is the OIDC issuer URL the cert's Fulcio extension must
/// match exactly. `subject_pattern` is a `globset`-style glob the
/// cert's OIDC subject must match — supports `*`, `?`, `[...]`, and
/// path-separator-aware semantics. For GitHub Actions workflow
/// identities the canonical pattern is
/// `https://github.com/OWNER/REPO/.github/workflows/release.yml@refs/tags/v*`.
pub struct CosignKeylessVerifier {
    name: String,
    issuer: String,
    subject_pattern: String,
    subject_glob: GlobMatcher,
    trusted_root: TrustedRoot,
}

impl std::fmt::Debug for CosignKeylessVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CosignKeylessVerifier")
            .field("name", &self.name)
            .field("issuer", &self.issuer)
            .field("subject_pattern", &self.subject_pattern)
            .finish()
    }
}

impl CosignKeylessVerifier {
    /// Construct using the embedded Sigstore production trust root.
    ///
    /// The trust root is shipped inside `sigstore-trust-root`; it
    /// contains the current Sigstore Fulcio CA chain, Rekor public
    /// keys, and Sigstore TSA roots. The bundle is a frozen-in-time
    /// snapshot — for long-running production deployments operators
    /// should layer Phase 4's `trust_root_bundle` config on top to
    /// pick up rotations.
    pub fn new(name: &str, issuer: &str, subject_pattern: &str) -> Result<Self, SigningError> {
        let glob = Glob::new(subject_pattern)
            .map_err(|e| SigningError::InvalidPolicy {
                message: format!(
                    "trusted signer '{name}': subject_pattern '{subject_pattern}' is not a valid glob: {e}",
                ),
            })?
            .compile_matcher();

        let trusted_root =
            TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT).map_err(|e| {
                SigningError::InvalidPolicy {
                    message: format!(
                        "trusted signer '{name}': failed to load embedded Sigstore production trust root: {e}",
                    ),
                }
            })?;

        Ok(Self {
            name: name.to_string(),
            issuer: issuer.to_string(),
            subject_pattern: subject_pattern.to_string(),
            subject_glob: glob,
            trusted_root,
        })
    }

    /// Construct against a caller-supplied trust root.
    ///
    /// Phase 4's `trust_root_bundle` config path passes a
    /// pre-loaded `TrustedRoot` through here; tests do the same with a
    /// fixture root so they don't drift if the bundled production
    /// root ages out.
    pub fn with_trust_root(
        name: &str,
        issuer: &str,
        subject_pattern: &str,
        trusted_root: TrustedRoot,
    ) -> Result<Self, SigningError> {
        let glob = Glob::new(subject_pattern)
            .map_err(|e| SigningError::InvalidPolicy {
                message: format!(
                    "trusted signer '{name}': subject_pattern '{subject_pattern}' is not a valid glob: {e}",
                ),
            })?
            .compile_matcher();
        Ok(Self {
            name: name.to_string(),
            issuer: issuer.to_string(),
            subject_pattern: subject_pattern.to_string(),
            subject_glob: glob,
            trusted_root,
        })
    }

    /// Borrow the configured issuer string — handy for tests and for
    /// telemetry that wants to bucket verifications by trust anchor.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Borrow the configured subject pattern.
    pub fn subject_pattern(&self) -> &str {
        &self.subject_pattern
    }
}

impl SchemaVerifier for CosignKeylessVerifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "cosign-keyless"
    }

    fn verify(
        &self,
        file_path: &Path,
        file_bytes: &[u8],
        signature: &[u8],
        _cert: Option<&[u8]>,
    ) -> Result<VerifiedIdentity, VerifyError> {
        // `<file>.sig` for cosign-keyless must be a Sigstore Bundle
        // JSON. Non-UTF-8 or non-bundle input is treated as
        // `MalformedSignature`, which the multi-scheme dispatch in
        // `policy::verify_with_any` uses as a "try the next verifier"
        // signal — so an ed25519 `.sig` flowing past this verifier
        // doesn't surface as a confusing cosign error.
        let bundle_str = std::str::from_utf8(signature).map_err(|e| {
            VerifyError::MalformedSignature {
                path: file_path.to_path_buf(),
                message: format!("cosign-keyless: signature is not valid UTF-8: {e}"),
            }
        })?;

        if !bundle_str.trim_start().starts_with('{') {
            return Err(VerifyError::MalformedSignature {
                path: file_path.to_path_buf(),
                message: "cosign-keyless: signature is not a JSON Sigstore Bundle".into(),
            });
        }

        let bundle = Bundle::from_json(bundle_str).map_err(|e| VerifyError::MalformedSignature {
            path: file_path.to_path_buf(),
            message: format!("cosign-keyless: not a valid Sigstore Bundle: {e}"),
        })?;

        let policy = VerificationPolicy::default().require_issuer(self.issuer.clone());

        let verifier = SigstoreVerifier::new(&self.trusted_root);
        let result = verifier
            .verify(file_bytes, &bundle, &policy)
            .map_err(|e| VerifyError::UntrustedSigner {
                path: file_path.to_path_buf(),
                reason: format!(
                    "cosign-keyless: verification failed against trust anchor '{}': {e}",
                    self.name,
                ),
            })?;

        let identity = result.identity.ok_or_else(|| VerifyError::UntrustedSigner {
            path: file_path.to_path_buf(),
            reason: format!(
                "cosign-keyless: certificate verified against '{}' but carries no OIDC subject (SAN)",
                self.name,
            ),
        })?;

        if !self.subject_glob.is_match(&identity) {
            return Err(VerifyError::UntrustedSigner {
                path: file_path.to_path_buf(),
                reason: format!(
                    "cosign-keyless: identity '{identity}' does not match subject_pattern '{}' for trust anchor '{}'",
                    self.subject_pattern, self.name,
                ),
            });
        }

        Ok(VerifiedIdentity {
            kind: "cosign-keyless",
            name: format!("{}@{}", identity, self.issuer),
        })
    }
}

/// Convenience function exported for tests and tooling that needs to
/// load a Sigstore trust root from a JSON file on disk (Phase 4
/// `trust_root_bundle`, fixture rotation drills). Wraps
/// [`TrustedRoot::from_json`] with a path-aware error.
pub fn load_trust_root_from_path(path: &Path) -> Result<TrustedRoot, SigningError> {
    let bytes = std::fs::read_to_string(path).map_err(|source| SigningError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    TrustedRoot::from_json(&bytes).map_err(|e| SigningError::InvalidPolicy {
        message: format!(
            "trust root at {} failed to parse: {e}",
            PathBuf::from(path).display(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_bytes() -> &'static [u8] {
        // Identical to `sigstore-verify`'s `cosign-v3-blob.txt`:
        // committed into our own test_data so the test stays
        // hermetic if the upstream fixture path ever changes.
        include_bytes!("../../test_data/cosign-v3-blob.txt")
    }

    fn fixture_bundle_str() -> &'static str {
        include_str!("../../test_data/cosign-v3-blob.sigstore.json")
    }

    #[test]
    fn new_rejects_invalid_glob() {
        let err = CosignKeylessVerifier::new("bad", "https://example", "[unclosed").unwrap_err();
        match err {
            SigningError::InvalidPolicy { message } => {
                assert!(message.contains("subject_pattern"));
            }
            other => panic!("expected InvalidPolicy, got {other:?}"),
        }
    }

    #[test]
    fn new_loads_production_trust_root() {
        let v = CosignKeylessVerifier::new(
            "release",
            "https://token.actions.githubusercontent.com",
            "https://github.com/org/repo/*",
        )
        .expect("production trust root should load");
        assert_eq!(v.kind(), "cosign-keyless");
        assert_eq!(v.name(), "release");
        assert_eq!(v.issuer(), "https://token.actions.githubusercontent.com");
        assert_eq!(v.subject_pattern(), "https://github.com/org/repo/*");
    }

    #[test]
    fn malformed_signature_for_non_json_bytes() {
        let v = CosignKeylessVerifier::new("t", "https://issuer", "*").unwrap();
        // Plausible ed25519 base64 — distinctively NOT a JSON bundle.
        let bad = b"AAAAbm90LWEtYnVuZGxlAAAA";
        let err = v
            .verify(Path::new("x.schema"), b"content", bad, None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::MalformedSignature { .. }));
    }

    #[test]
    fn malformed_signature_for_invalid_utf8() {
        let v = CosignKeylessVerifier::new("t", "https://issuer", "*").unwrap();
        let bad = [0xff, 0xfe, 0xfd];
        let err = v
            .verify(Path::new("x.schema"), b"content", &bad, None)
            .unwrap_err();
        match err {
            VerifyError::MalformedSignature { message, .. } => {
                assert!(message.contains("UTF-8"));
            }
            other => panic!("expected MalformedSignature, got {other:?}"),
        }
    }

    #[test]
    fn malformed_signature_for_bundle_that_does_not_parse() {
        let v = CosignKeylessVerifier::new("t", "https://issuer", "*").unwrap();
        let bad = br#"{"mediaType":"text/plain","unrelated":true}"#;
        let err = v
            .verify(Path::new("x.schema"), b"content", bad, None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::MalformedSignature { .. }));
    }

    #[test]
    fn verify_accepts_real_cosign_bundle() {
        // The cosign-v3-blob fixture is a real Sigstore Bundle signed
        // by an interactive OAuth identity (issuer
        // `https://github.com/login/oauth`). It verifies against the
        // production trust root because the cert chain + Rekor entry
        // are preserved in the bundle.
        let v = CosignKeylessVerifier::new(
            "fixture",
            "https://github.com/login/oauth",
            "*",
        )
        .unwrap();
        let id = v
            .verify(
                Path::new("blob.txt"),
                fixture_bytes(),
                fixture_bundle_str().as_bytes(),
                None,
            )
            .expect("cosign-v3-blob fixture must verify against the production trust root");
        assert_eq!(id.kind, "cosign-keyless");
        // The fixture's OIDC subject is an email — the formatted
        // identity must contain it and end with the issuer.
        assert!(
            id.name.ends_with("@https://github.com/login/oauth"),
            "expected suffix @issuer in {}",
            id.name,
        );
    }

    #[test]
    fn verify_rejects_wrong_issuer() {
        // Same fixture but configured with a different issuer than
        // the cert carries — sigstore-verify rejects with an issuer
        // mismatch.
        let v = CosignKeylessVerifier::new(
            "fixture",
            "https://token.actions.githubusercontent.com",
            "*",
        )
        .unwrap();
        let err = v
            .verify(
                Path::new("blob.txt"),
                fixture_bytes(),
                fixture_bundle_str().as_bytes(),
                None,
            )
            .unwrap_err();
        match err {
            VerifyError::UntrustedSigner { reason, .. } => {
                assert!(reason.to_lowercase().contains("issuer"), "{reason}");
            }
            other => panic!("expected UntrustedSigner, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_subject_pattern_mismatch() {
        // Issuer matches; pattern does not. Identity match check
        // runs *after* sigstore-verify accepts so the failure is
        // about the schemaforge layer, not the cert chain.
        let v = CosignKeylessVerifier::new(
            "fixture",
            "https://github.com/login/oauth",
            "https://example.invalid/strictly-not-matching",
        )
        .unwrap();
        let err = v
            .verify(
                Path::new("blob.txt"),
                fixture_bytes(),
                fixture_bundle_str().as_bytes(),
                None,
            )
            .unwrap_err();
        match err {
            VerifyError::UntrustedSigner { reason, .. } => {
                assert!(reason.contains("subject_pattern"), "{reason}");
            }
            other => panic!("expected UntrustedSigner, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_tampered_artifact() {
        // Verify against a *different* artifact than was signed —
        // sigstore-verify catches via the rekor-hashedrekord
        // consistency step (the bundle pins the digest of the
        // original blob).
        let v = CosignKeylessVerifier::new("fixture", "https://github.com/login/oauth", "*")
            .unwrap();
        let tampered = b"this is not the content that was signed";
        let err = v
            .verify(
                Path::new("blob.txt"),
                tampered,
                fixture_bundle_str().as_bytes(),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, VerifyError::UntrustedSigner { .. }));
    }

    #[test]
    fn subject_glob_supports_wildcard_segments() {
        // Build directly off the in-process glob to keep this test
        // hermetic — exercises just the pattern surface schemaforge
        // owns.
        let v = CosignKeylessVerifier::new(
            "release",
            "https://token.actions.githubusercontent.com",
            "https://github.com/org/repo/.github/workflows/release.yml@refs/tags/v*",
        )
        .unwrap();
        assert!(v.subject_glob.is_match(
            "https://github.com/org/repo/.github/workflows/release.yml@refs/tags/v0.42.0"
        ));
        assert!(!v.subject_glob.is_match(
            "https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main"
        ));
        assert!(!v.subject_glob.is_match(
            "https://github.com/other/repo/.github/workflows/release.yml@refs/tags/v0.42.0"
        ));
    }
}
