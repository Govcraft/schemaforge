//! Resolved verification policy + the top-level `verify_files` entry
//! point used by the CLI.
//!
//! [`SigningConfig`] is the operator-facing TOML shape;
//! [`VerifyPolicy`] is what the rest of the code base depends on. It
//! holds an already-built list of `dyn SchemaVerifier` so the hot path
//! (one call per file per command) never re-parses keys.

use std::path::{Path, PathBuf};

use crate::config::{SigningConfig, SigningMode, TrustedSigner};
use crate::error::{SigningError, VerifyError};
use crate::hash::sha256_hex;
use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
use crate::verifier::{SchemaVerifier, VerifiedIdentity};
use crate::verifiers::cosign::{load_trust_root_from_path, CosignKeylessVerifier};
use crate::verifiers::ed25519::{signature_path_for, Ed25519Verifier};
use crate::verifiers::ssh::SshAllowedSignersVerifier;

/// Resolved policy: mode + verifier instances.
pub struct VerifyPolicy {
    mode: SigningMode,
    verifiers: Vec<Box<dyn SchemaVerifier>>,
    manifest_filename: String,
}

impl std::fmt::Debug for VerifyPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyPolicy")
            .field("mode", &self.mode)
            .field("manifest_filename", &self.manifest_filename)
            .field(
                "verifiers",
                &self
                    .verifiers
                    .iter()
                    .map(|v| format!("{}:{}", v.kind(), v.name()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl VerifyPolicy {
    /// Build from a deserialized [`SigningConfig`].
    ///
    /// Each `[[trusted_signers]]` entry constructs the matching
    /// verifier eagerly so any malformed key / glob / unreadable path
    /// surfaces at startup rather than on the first verification
    /// attempt.
    pub fn from_config(config: &SigningConfig) -> Result<Self, SigningError> {
        let mut verifiers: Vec<Box<dyn SchemaVerifier>> = Vec::new();

        // For airgap / SCIF deployments, the operator points
        // `trust_root_bundle` at a pre-fetched Sigstore TUF snapshot
        // (refreshed on a connected host via
        // `schemaforge trust-bundle refresh`). When unset, every
        // cosign-keyless verifier falls back to the embedded
        // production snapshot baked into `sigstore-trust-root`. We
        // load the override exactly once and clone it into each
        // cosign verifier so a directory with several cosign anchors
        // doesn't re-parse the (~150 KB) JSON per anchor.
        let trust_root_override = match &config.trust_root_bundle {
            Some(path) => Some(load_trust_root_from_path(path)?),
            None => None,
        };

        for signer in &config.trusted_signers {
            match signer {
                TrustedSigner::Ed25519 {
                    name,
                    public_key_b64,
                } => {
                    verifiers.push(Box::new(Ed25519Verifier::from_b64(name, public_key_b64)?));
                }
                TrustedSigner::Ed25519PemFile { name, path } => {
                    verifiers.push(Box::new(Ed25519Verifier::from_pem_file(name, path)?));
                }
                TrustedSigner::SshAllowedSigners { name, path } => {
                    verifiers.push(Box::new(SshAllowedSignersVerifier::from_file(name, path)?));
                }
                TrustedSigner::CosignKeyless {
                    name,
                    issuer,
                    subject_pattern,
                } => {
                    let label = name.as_deref().unwrap_or("cosign-keyless");
                    let verifier = match &trust_root_override {
                        Some(root) => CosignKeylessVerifier::with_trust_root(
                            label,
                            issuer,
                            subject_pattern,
                            root.clone(),
                        )?,
                        None => CosignKeylessVerifier::new(label, issuer, subject_pattern)?,
                    };
                    verifiers.push(Box::new(verifier));
                }
            }
        }

        if config.mode.is_active() && verifiers.is_empty() {
            return Err(SigningError::InvalidPolicy {
                message: format!(
                    "signing.mode = \"{:?}\" requires at least one entry under \
                     [[schema_forge.signing.trusted_signers]]",
                    config.mode
                )
                .to_lowercase(),
            });
        }

        Ok(Self {
            mode: config.mode,
            verifiers,
            manifest_filename: config
                .manifest_filename
                .clone()
                .unwrap_or_else(|| MANIFEST_FILE_NAME.to_string()),
        })
    }

    /// The trivial all-allow policy. Used when signing is intentionally
    /// disabled at the call site (e.g., the `parse` subcommand reading
    /// unsigned content for diagnostics).
    pub fn off() -> Self {
        Self {
            mode: SigningMode::Off,
            verifiers: Vec::new(),
            manifest_filename: MANIFEST_FILE_NAME.to_string(),
        }
    }

    /// Current mode (off / warn / enforce).
    pub fn mode(&self) -> SigningMode {
        self.mode
    }

    /// Number of trust anchors loaded.
    pub fn signer_count(&self) -> usize {
        self.verifiers.len()
    }

    /// Manifest filename in use.
    pub fn manifest_filename(&self) -> &str {
        &self.manifest_filename
    }

    /// Verify every file in `files` against this policy.
    ///
    /// Returns:
    /// - `Ok(report)` with `report.overall_ok == true` when every check
    ///   passed (including under `mode = "warn"` if all files actually
    ///   verified).
    /// - `Ok(report)` with `report.overall_ok == false` when something
    ///   failed but the mode is non-strict (`off` or `warn`); the
    ///   caller surfaces failures as warnings and continues.
    /// - `Err(VerifyError)` when `mode = "enforce"` and any check
    ///   failed; the CLI propagates this as a hard error.
    ///
    /// `manifest_dir` is the directory the manifest is expected to
    /// live in. `files` must all be inside that directory (or a
    /// subtree of it).
    pub fn verify_files(
        &self,
        manifest_dir: &Path,
        files: &[PathBuf],
    ) -> Result<VerifyReport, VerifyError> {
        if matches!(self.mode, SigningMode::Off) {
            return Ok(VerifyReport::skipped(
                manifest_dir.join(&self.manifest_filename),
                files,
            ));
        }
        match self.verify_inner(manifest_dir, files) {
            Ok(report) => Ok(report),
            Err(e) => {
                if self.mode.is_strict() {
                    Err(e)
                } else {
                    // Non-strict: bundle the failure into the report
                    // and return it so the caller can log + proceed.
                    Ok(VerifyReport {
                        manifest_path: manifest_dir.join(&self.manifest_filename),
                        manifest_signer: None,
                        files: vec![FileVerifyResult {
                            path: PathBuf::new(),
                            outcome: FileVerifyOutcome::Failed {
                                reason: e.to_string(),
                            },
                        }],
                        overall_ok: false,
                    })
                }
            }
        }
    }

    fn verify_inner(
        &self,
        manifest_dir: &Path,
        files: &[PathBuf],
    ) -> Result<VerifyReport, VerifyError> {
        let manifest_path = manifest_dir.join(&self.manifest_filename);
        let manifest_bytes =
            std::fs::read(&manifest_path).map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => VerifyError::MissingManifest {
                    path: manifest_path.clone(),
                },
                _ => VerifyError::Io {
                    path: manifest_path.clone(),
                    source,
                },
            })?;

        let manifest_sig_path = signature_path_for(&manifest_path);
        let manifest_sig =
            std::fs::read(&manifest_sig_path).map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => VerifyError::MissingSignature {
                    path: manifest_path.clone(),
                },
                _ => VerifyError::Io {
                    path: manifest_sig_path.clone(),
                    source,
                },
            })?;

        let manifest_signer = self.verify_with_any(&manifest_path, &manifest_bytes, &manifest_sig)?;

        let manifest = Manifest::from_bytes(&manifest_path, &manifest_bytes).map_err(|e| {
            VerifyError::UntrustedSigner {
                path: manifest_path.clone(),
                reason: format!("manifest signature verified but content is malformed: {e}"),
            }
        })?;

        // Disk/manifest mutual-coverage check before per-file work — if
        // a file is missing or extra, no point hashing the rest.
        manifest.validate_against_disk(manifest_dir, files)?;

        let mut file_results = Vec::with_capacity(files.len());
        for file in files {
            let result = self.verify_one_file(manifest_dir, &manifest, file)?;
            file_results.push(result);
        }

        Ok(VerifyReport {
            manifest_path,
            manifest_signer: Some(manifest_signer),
            files: file_results,
            overall_ok: true,
        })
    }

    fn verify_one_file(
        &self,
        manifest_dir: &Path,
        manifest: &Manifest,
        file: &Path,
    ) -> Result<FileVerifyResult, VerifyError> {
        let file_bytes = std::fs::read(file).map_err(|source| VerifyError::Io {
            path: file.to_path_buf(),
            source,
        })?;

        // Pinned hash check first — even if the per-file signature
        // verifies, mismatched content means someone signed *different*
        // bytes than the manifest pins (replay / out-of-band edit).
        let rel = relative_path(manifest_dir, file).ok_or_else(|| VerifyError::FileNotInManifest {
            path: file.to_path_buf(),
        })?;
        let expected = manifest.pinned_hash(&rel).ok_or_else(|| {
            VerifyError::FileNotInManifest {
                path: file.to_path_buf(),
            }
        })?;
        let actual = sha256_hex(&file_bytes);
        if expected != actual {
            return Err(VerifyError::HashMismatch {
                path: file.to_path_buf(),
                expected: expected.to_string(),
                actual,
            });
        }

        let sig_path = signature_path_for(file);
        let sig_bytes = std::fs::read(&sig_path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => VerifyError::MissingSignature {
                path: file.to_path_buf(),
            },
            _ => VerifyError::Io {
                path: sig_path.clone(),
                source,
            },
        })?;

        let identity = self.verify_with_any(file, &file_bytes, &sig_bytes)?;

        Ok(FileVerifyResult {
            path: file.to_path_buf(),
            outcome: FileVerifyOutcome::Verified { identity },
        })
    }

    /// Try every loaded verifier; return the first that accepts.
    ///
    /// Two failure shapes get folded together so multi-scheme policies
    /// behave sensibly:
    ///
    /// - `UntrustedSigner` from one verifier just means "this anchor
    ///   said no" — we record the reason and move on to the next.
    /// - `MalformedSignature` from one verifier means "the bytes don't
    ///   look like this scheme's format" — also a try-next signal, not
    ///   a hard error, because multi-scheme deployments writing both
    ///   raw-ed25519 and SSH signatures produce different on-disk
    ///   formats and each verifier rejects the other scheme's bytes as
    ///   malformed.
    ///
    /// If every verifier rejected, we surface the last `UntrustedSigner`
    /// reason if any verifier produced one; otherwise the last
    /// malformed-format complaint. That gives the operator the most
    /// specific diagnostic — a real signer mismatch beats a "wrong
    /// file format" message when both could apply.
    fn verify_with_any(
        &self,
        path: &Path,
        bytes: &[u8],
        signature: &[u8],
    ) -> Result<VerifiedIdentity, VerifyError> {
        let mut last_untrusted: Option<String> = None;
        let mut last_malformed: Option<String> = None;
        for v in &self.verifiers {
            match v.verify(path, bytes, signature, None) {
                Ok(id) => return Ok(id),
                Err(VerifyError::UntrustedSigner { reason, .. }) => {
                    last_untrusted = Some(reason);
                }
                Err(VerifyError::MalformedSignature { message, .. }) => {
                    last_malformed = Some(message);
                }
                Err(other) => return Err(other),
            }
        }
        if let Some(reason) = last_untrusted {
            return Err(VerifyError::UntrustedSigner {
                path: path.to_path_buf(),
                reason,
            });
        }
        if let Some(message) = last_malformed {
            return Err(VerifyError::MalformedSignature {
                path: path.to_path_buf(),
                message,
            });
        }
        Err(VerifyError::UntrustedSigner {
            path: path.to_path_buf(),
            reason: "no trusted signers configured".into(),
        })
    }
}

/// Top-level outcome for one verification call.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub manifest_path: PathBuf,
    pub manifest_signer: Option<VerifiedIdentity>,
    pub files: Vec<FileVerifyResult>,
    /// `true` when every file verified. Always `false` after a
    /// non-strict-mode failure so the caller can still log the
    /// problem.
    pub overall_ok: bool,
}

impl VerifyReport {
    fn skipped(manifest_path: PathBuf, files: &[PathBuf]) -> Self {
        Self {
            manifest_path,
            manifest_signer: None,
            files: files
                .iter()
                .map(|p| FileVerifyResult {
                    path: p.clone(),
                    outcome: FileVerifyOutcome::Skipped,
                })
                .collect(),
            overall_ok: true,
        }
    }
}

/// Per-file outcome inside a [`VerifyReport`].
#[derive(Debug, Clone)]
pub struct FileVerifyResult {
    pub path: PathBuf,
    pub outcome: FileVerifyOutcome,
}

/// The verification outcome for one file.
#[derive(Debug, Clone)]
pub enum FileVerifyOutcome {
    Verified { identity: VerifiedIdentity },
    Skipped,
    Failed { reason: String },
}

/// Express `target` as a `/`-separated relative path under `base`,
/// or `None` when it isn't.
fn relative_path(base: &Path, target: &Path) -> Option<String> {
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let rel = target.strip_prefix(&base).ok()?;
    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        if let std::path::Component::Normal(s) = component {
            parts.push(s.to_string_lossy().into_owned());
        } else {
            return None;
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::{sign_directory, Ed25519Signer};
    use tempfile::tempdir;

    fn fixed_signer() -> Ed25519Signer {
        Ed25519Signer::from_seed_bytes(&[0x42; 32])
    }

    fn config_with_signer(signer: &Ed25519Signer, mode: SigningMode) -> SigningConfig {
        SigningConfig {
            mode,
            trusted_signers: vec![TrustedSigner::Ed25519 {
                name: "primary".into(),
                public_key_b64: signer.public_key_b64_raw(),
            }],
            manifest_filename: None,
            trust_root_bundle: None,
        }
    }

    /// Build a dir, sign it with the fixture key, return the dir
    /// plus the list of `.schema` files.
    fn signed_dir() -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.schema");
        let b = dir.path().join("b.schema");
        std::fs::write(&a, b"schema A {}").unwrap();
        std::fs::write(&b, b"schema B {}").unwrap();
        let files = vec![a, b];
        sign_directory(dir.path(), &files, &fixed_signer(), None).unwrap();
        (dir, files)
    }

    #[test]
    fn off_mode_skips_everything() {
        let policy = VerifyPolicy::off();
        let report = policy.verify_files(Path::new("/nonexistent"), &[]).unwrap();
        assert!(report.overall_ok);
    }

    #[test]
    fn enforce_mode_accepts_signed_directory() {
        let (dir, files) = signed_dir();
        let cfg = config_with_signer(&fixed_signer(), SigningMode::Enforce);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let report = policy.verify_files(dir.path(), &files).unwrap();
        assert!(report.overall_ok);
        assert!(report.manifest_signer.is_some());
        assert_eq!(report.files.len(), 2);
        for f in &report.files {
            assert!(matches!(f.outcome, FileVerifyOutcome::Verified { .. }));
        }
    }

    #[test]
    fn enforce_mode_rejects_tampered_file() {
        let (dir, files) = signed_dir();
        // Tamper with one schema after signing.
        std::fs::write(&files[0], b"schema A { tampered: bool }").unwrap();
        let cfg = config_with_signer(&fixed_signer(), SigningMode::Enforce);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let err = policy.verify_files(dir.path(), &files).unwrap_err();
        // Hash check fires before per-file signature check.
        assert!(matches!(err, VerifyError::HashMismatch { .. }));
    }

    #[test]
    fn enforce_mode_rejects_missing_signature() {
        let (dir, files) = signed_dir();
        // Remove the per-file sig so the verifier sees a signed
        // manifest + unsigned schema.
        std::fs::remove_file(signature_path_for(&files[0])).unwrap();
        let cfg = config_with_signer(&fixed_signer(), SigningMode::Enforce);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let err = policy.verify_files(dir.path(), &files).unwrap_err();
        assert!(matches!(err, VerifyError::MissingSignature { .. }));
    }

    #[test]
    fn enforce_mode_rejects_extra_file_on_disk() {
        let (dir, mut files) = signed_dir();
        let rogue = dir.path().join("rogue.schema");
        std::fs::write(&rogue, b"schema Rogue {}").unwrap();
        files.push(rogue);
        let cfg = config_with_signer(&fixed_signer(), SigningMode::Enforce);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let err = policy.verify_files(dir.path(), &files).unwrap_err();
        assert!(matches!(err, VerifyError::FileNotInManifest { .. }));
    }

    #[test]
    fn enforce_mode_rejects_missing_manifest() {
        let (dir, files) = signed_dir();
        std::fs::remove_file(dir.path().join(MANIFEST_FILE_NAME)).unwrap();
        let cfg = config_with_signer(&fixed_signer(), SigningMode::Enforce);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let err = policy.verify_files(dir.path(), &files).unwrap_err();
        assert!(matches!(err, VerifyError::MissingManifest { .. }));
    }

    #[test]
    fn warn_mode_does_not_propagate_failures() {
        let (dir, files) = signed_dir();
        std::fs::write(&files[0], b"tampered").unwrap();
        let cfg = config_with_signer(&fixed_signer(), SigningMode::Warn);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        // Verification ran, reported failure, but did not bubble.
        let report = policy.verify_files(dir.path(), &files).unwrap();
        assert!(!report.overall_ok);
    }

    #[test]
    fn enforce_mode_rejects_signature_from_wrong_key() {
        let (dir, files) = signed_dir();
        // Trust a *different* key.
        let other = Ed25519Signer::from_seed_bytes(&[0x77; 32]);
        let cfg = config_with_signer(&other, SigningMode::Enforce);
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let err = policy.verify_files(dir.path(), &files).unwrap_err();
        assert!(matches!(err, VerifyError::UntrustedSigner { .. }));
    }

    #[test]
    fn from_config_rejects_empty_signer_list_under_enforce() {
        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![],
            manifest_filename: None,
            trust_root_bundle: None,
        };
        let err = VerifyPolicy::from_config(&cfg).unwrap_err();
        assert!(matches!(err, SigningError::InvalidPolicy { .. }));
    }

    #[test]
    fn from_config_accepts_cosign_keyless_signer() {
        // Phase 3: cosign-keyless is now a first-class verifier kind.
        // Construction should succeed against any well-formed glob and
        // issuer; verification correctness is exercised in
        // `verifiers::cosign::tests`.
        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![TrustedSigner::CosignKeyless {
                name: Some("release-pipeline".into()),
                issuer: "https://token.actions.githubusercontent.com".into(),
                subject_pattern: "https://github.com/example/repo/.github/workflows/*@refs/tags/v*".into(),
            }],
            manifest_filename: None,
            trust_root_bundle: None,
        };
        let policy = VerifyPolicy::from_config(&cfg).expect("policy should build");
        assert_eq!(policy.signer_count(), 1);
    }

    #[test]
    fn from_config_loads_trust_root_bundle_when_set() {
        // Write the fixture trust-root JSON to a temp file and point
        // `trust_root_bundle` at it. Verifier construction must
        // succeed AND the load must visibly happen — exercised
        // indirectly by ensuring an unreadable bundle path errors.
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("trust_root.json");
        std::fs::write(
            &bundle_path,
            include_str!("../test_data/trust_root_fixture.json"),
        )
        .unwrap();

        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![TrustedSigner::CosignKeyless {
                name: Some("release".into()),
                issuer: "https://token.actions.githubusercontent.com".into(),
                subject_pattern: "*".into(),
            }],
            manifest_filename: None,
            trust_root_bundle: Some(bundle_path),
        };
        let policy = VerifyPolicy::from_config(&cfg).expect("policy should build");
        assert_eq!(policy.signer_count(), 1);
    }

    #[test]
    fn from_config_rejects_unreadable_trust_root_bundle() {
        // Pointing at a nonexistent path must fail at policy build
        // rather than silently fall back to the embedded snapshot —
        // an operator who set `trust_root_bundle` expected that
        // exact file to drive verification.
        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![TrustedSigner::CosignKeyless {
                name: None,
                issuer: "https://issuer".into(),
                subject_pattern: "*".into(),
            }],
            manifest_filename: None,
            trust_root_bundle: Some(PathBuf::from("/nonexistent/airgap/trust_root.json")),
        };
        let err = VerifyPolicy::from_config(&cfg).unwrap_err();
        // Either Io (path doesn't exist) or InvalidPolicy (path
        // exists but unparseable) is acceptable here — the contract
        // is "fail loud."
        assert!(
            matches!(err, SigningError::Io { .. } | SigningError::InvalidPolicy { .. }),
            "expected Io or InvalidPolicy, got {err:?}",
        );
    }

    #[test]
    fn trust_root_bundle_override_verifies_cosign_fixture() {
        // End-to-end: trust_root_bundle points at our fixture JSON;
        // the cosign-v3-blob bundle must verify through the
        // operator-supplied trust root. Together with the
        // `_loads_trust_root_bundle_when_set` test, this proves the
        // override path is wired through every cosign verifier built
        // by `from_config` (not just the first or the one with a
        // specific name).
        use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
        use crate::signer::DirectorySigner;
        use crate::verifiers::ed25519::signature_path_for;

        let cosign_blob: &[u8] = include_bytes!("../test_data/cosign-v3-blob.txt");
        let cosign_bundle: &str = include_str!("../test_data/cosign-v3-blob.sigstore.json");

        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("trust_root.json");
        // Write the same trust-root JSON the embedded
        // `SIGSTORE_PRODUCTION_TRUSTED_ROOT` constant carries, but to
        // disk — so we exercise the file-read + JSON-parse path while
        // still using a trust root that's known to verify the
        // cosign-v3-blob fixture.
        std::fs::write(
            &bundle_path,
            sigstore_trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT,
        )
        .unwrap();

        let schema_path = dir.path().join("blob.schema");
        std::fs::write(&schema_path, cosign_blob).unwrap();
        std::fs::write(signature_path_for(&schema_path), cosign_bundle).unwrap();

        let manifest = Manifest::build(dir.path(), std::slice::from_ref(&schema_path)).unwrap();
        let manifest_bytes = manifest.to_toml_bytes().unwrap();
        let manifest_path = dir.path().join(MANIFEST_FILE_NAME);
        std::fs::write(&manifest_path, &manifest_bytes).unwrap();

        let manifest_signer = Ed25519Signer::from_seed_bytes(&[0xCD; 32]);
        let manifest_sig = manifest_signer.sign_to_sig_bytes(&manifest_bytes).unwrap();
        std::fs::write(signature_path_for(&manifest_path), manifest_sig).unwrap();

        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![
                TrustedSigner::Ed25519 {
                    name: "manifest-key".into(),
                    public_key_b64: manifest_signer.public_key_b64_raw(),
                },
                TrustedSigner::CosignKeyless {
                    name: Some("airgap-fixture".into()),
                    issuer: "https://github.com/login/oauth".into(),
                    subject_pattern: "*".into(),
                },
            ],
            manifest_filename: None,
            trust_root_bundle: Some(bundle_path),
        };
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let report = policy
            .verify_files(dir.path(), std::slice::from_ref(&schema_path))
            .unwrap();
        assert!(report.overall_ok);
    }

    #[test]
    fn from_config_rejects_cosign_keyless_with_bad_glob() {
        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![TrustedSigner::CosignKeyless {
                name: None,
                issuer: "https://issuer".into(),
                subject_pattern: "[unclosed-bracket".into(),
            }],
            manifest_filename: None,
            trust_root_bundle: None,
        };
        let err = VerifyPolicy::from_config(&cfg).unwrap_err();
        assert!(matches!(err, SigningError::InvalidPolicy { .. }));
    }

    /// Multi-scheme drill: one schema file signed cosign-keyless, the
    /// manifest signed ed25519. Exercises the [`verify_with_any`]
    /// fall-through where each per-artifact signature only matches
    /// one of the configured verifiers — the OR-semantics + try-next
    /// dispatch is the load-bearing piece that lets operators rotate
    /// between schemes without atomically re-signing every file.
    #[test]
    fn mixed_scheme_directory_verifies_when_both_anchors_present() {
        use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
        use crate::signer::DirectorySigner;
        use crate::verifiers::ed25519::signature_path_for;

        let cosign_blob: &[u8] =
            include_bytes!("../test_data/cosign-v3-blob.txt");
        let cosign_bundle: &str =
            include_str!("../test_data/cosign-v3-blob.sigstore.json");

        let dir = tempdir().unwrap();
        // Schema file content == the cosign fixture's signed blob, so
        // the fixture bundle is a valid signature over the file.
        let schema_path = dir.path().join("blob.schema");
        std::fs::write(&schema_path, cosign_blob).unwrap();
        std::fs::write(signature_path_for(&schema_path), cosign_bundle).unwrap();

        // Manifest signed by an ed25519 signer the policy trusts.
        let manifest = Manifest::build(dir.path(), std::slice::from_ref(&schema_path)).unwrap();
        let manifest_bytes = manifest.to_toml_bytes().unwrap();
        let manifest_path = dir.path().join(MANIFEST_FILE_NAME);
        std::fs::write(&manifest_path, &manifest_bytes).unwrap();

        let manifest_signer = Ed25519Signer::from_seed_bytes(&[0xAB; 32]);
        let manifest_sig = manifest_signer
            .sign_to_sig_bytes(&manifest_bytes)
            .unwrap();
        std::fs::write(signature_path_for(&manifest_path), manifest_sig).unwrap();

        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![
                TrustedSigner::Ed25519 {
                    name: "manifest-key".into(),
                    public_key_b64: manifest_signer.public_key_b64_raw(),
                },
                TrustedSigner::CosignKeyless {
                    name: Some("fixture".into()),
                    issuer: "https://github.com/login/oauth".into(),
                    subject_pattern: "*".into(),
                },
            ],
            manifest_filename: None,
            trust_root_bundle: None,
        };
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let report = policy
            .verify_files(dir.path(), std::slice::from_ref(&schema_path))
            .unwrap();
        assert!(report.overall_ok);
        // The schema's outcome must trace to the cosign verifier, not
        // ed25519 — otherwise the fall-through would have masked a
        // bug where ed25519 spuriously accepted bundle JSON.
        let outcome = &report.files[0].outcome;
        match outcome {
            FileVerifyOutcome::Verified { identity } => {
                assert_eq!(identity.kind, "cosign-keyless");
            }
            other => panic!("expected Verified via cosign, got {other:?}"),
        }
        // Manifest signer must trace back to the ed25519 anchor.
        let manifest_signer_id = report.manifest_signer.unwrap();
        assert_eq!(manifest_signer_id.kind, "ed25519");
        assert_eq!(manifest_signer_id.name, "manifest-key");
    }

    #[test]
    fn rotating_to_additional_key_keeps_old_signatures_valid() {
        let (dir, files) = signed_dir();
        // Trust the original key *and* an unrelated new key. Existing
        // signatures from the original key must still pass — this is
        // the OR-semantics rotation story.
        let new_key = Ed25519Signer::from_seed_bytes(&[0x99; 32]);
        let cfg = SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![
                TrustedSigner::Ed25519 {
                    name: "old".into(),
                    public_key_b64: fixed_signer().public_key_b64_raw(),
                },
                TrustedSigner::Ed25519 {
                    name: "new".into(),
                    public_key_b64: new_key.public_key_b64_raw(),
                },
            ],
            manifest_filename: None,
            trust_root_bundle: None,
        };
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let report = policy.verify_files(dir.path(), &files).unwrap();
        assert!(report.overall_ok);
        for f in &report.files {
            match &f.outcome {
                FileVerifyOutcome::Verified { identity } => {
                    assert_eq!(identity.name, "old");
                }
                other => panic!("expected Verified, got {other:?}"),
            }
        }
    }
}
