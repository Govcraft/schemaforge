//! Signing helpers used by `schemaforge sign`.
//!
//! The verifier side is the security-critical surface — the signer
//! side just produces correctly-shaped artefacts the verifier will
//! later accept. Anything an operator can do with raw `cosign sign-blob`
//! or `ssh-keygen -Y sign` we want to be able to do here too, with
//! consistent paths and no per-tool muscle memory.
//!
//! Three concrete signers ship today:
//!
//! - [`Ed25519Signer`]: raw Ed25519 over a PKCS#8 PEM key. Smallest
//!   surface; pairs with [`crate::verifiers::ed25519::Ed25519Verifier`].
//! - [`SshSigner`]: SSHSIG (`ssh-keygen -Y sign`) over an OpenSSH
//!   private key. Pairs with
//!   [`crate::verifiers::ssh::SshAllowedSignersVerifier`].
//! - [`CosignKeylessSigner`]: shells out to `cosign sign-blob --bundle`
//!   so each `.sig` becomes a Sigstore Bundle JSON ready for
//!   [`crate::verifiers::cosign::CosignKeylessVerifier`]. Requires
//!   `cosign` in `PATH` plus a working OIDC token source (e.g. GitHub
//!   Actions' `ACTIONS_ID_TOKEN_REQUEST_TOKEN`/`URL`).
//!
//! All three implement the [`DirectorySigner`] trait so [`sign_directory`]
//! can drive any of them without conditional logic at the call site.

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{Signer, SigningKey};
use ssh_key::{HashAlg, LineEnding, PrivateKey, SshSig};

use crate::error::SigningError;
use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
use crate::verifiers::ed25519::{encode_signature_b64, signature_path_for};
use crate::verifiers::ssh::SSHSIG_NAMESPACE;

/// Common signing surface used by [`sign_directory`].
///
/// Each implementor writes its own on-disk signature format (base64 for
/// raw Ed25519, PEM-armored SSHSIG for ssh-keygen-style signatures);
/// the trait abstracts only "produce the bytes to put in `<file>.sig`."
pub trait DirectorySigner {
    /// Produce the bytes that should be written verbatim into the
    /// `<file>.sig` companion. Includes any encoding (base64 / PEM)
    /// the corresponding verifier will reverse.
    fn sign_to_sig_bytes(&self, message: &[u8]) -> Result<Vec<u8>, SigningError>;

    /// Stable scheme tag, matching [`crate::verifier::VerifiedIdentity::kind`].
    fn kind(&self) -> &'static str;
}

/// Raw ed25519 signer paired with the path layout
/// [`crate::verifiers::ed25519::Ed25519Verifier`] expects.
pub struct Ed25519Signer {
    signing_key: SigningKey,
}

impl Ed25519Signer {
    /// Load from a PKCS#8 PEM file (the format
    /// [`Ed25519Signer::write_keypair`] produces, and the format
    /// `openssl genpkey -algorithm ed25519` produces by default).
    pub fn from_pem_file(path: &Path) -> Result<Self, SigningError> {
        let pem = std::fs::read_to_string(path).map_err(|source| SigningError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let signing_key =
            SigningKey::from_pkcs8_pem(&pem).map_err(|e| SigningError::InvalidKey {
                name: path.display().to_string(),
                message: format!("PKCS#8 PEM parse failed: {e}"),
            })?;
        Ok(Self { signing_key })
    }

    /// Load from raw 32-byte seed bytes (the format the test suite uses
    /// and what `schemaforge token init-key` style commands could
    /// generate). Production operators should prefer PEM.
    pub fn from_seed_bytes(seed: &[u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(seed),
        }
    }

    /// Borrow the verifying key — useful for printing a public key the
    /// operator should put into the trust policy.
    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Public key as base64-encoded SubjectPublicKeyInfo DER. This is
    /// the form the operator pastes into `[[schema_forge.signing.
    /// trusted_signers]] public_key_b64 = "..."`.
    pub fn public_key_b64_spki(&self) -> Result<String, SigningError> {
        let der = self
            .signing_key
            .verifying_key()
            .to_public_key_der()
            .map_err(|e| SigningError::Other(format!("failed to encode public key as DER: {e}")))?
            .into_vec();
        Ok(base64::engine::general_purpose::STANDARD.encode(&der))
    }

    /// Public key as base64-encoded raw 32 bytes. Smaller, also
    /// accepted by [`crate::verifiers::ed25519::Ed25519Verifier::from_b64`].
    pub fn public_key_b64_raw(&self) -> String {
        base64::engine::general_purpose::STANDARD
            .encode(self.signing_key.verifying_key().as_bytes())
    }

    /// Sign `bytes` and return the base64-encoded 64-byte signature.
    pub fn sign_b64(&self, bytes: &[u8]) -> String {
        encode_signature_b64(&self.signing_key.sign(bytes))
    }

    /// Write a PKCS#8 PEM-encoded private key.
    pub fn write_keypair(&self, path: &Path) -> Result<(), SigningError> {
        let pem = self
            .signing_key
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .map_err(|e| SigningError::Other(format!("failed to encode PKCS#8: {e}")))?
            .to_string();
        std::fs::write(path, pem).map_err(|source| SigningError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

impl DirectorySigner for Ed25519Signer {
    fn sign_to_sig_bytes(&self, message: &[u8]) -> Result<Vec<u8>, SigningError> {
        Ok(self.sign_b64(message).into_bytes())
    }

    fn kind(&self) -> &'static str {
        "ed25519"
    }
}

/// SSHSIG signer driven by an OpenSSH private key (e.g.
/// `~/.ssh/id_ed25519`).
///
/// Produces PEM-armored signatures (`-----BEGIN SSH SIGNATURE-----`)
/// identical to what `ssh-keygen -Y sign -n schema-forge-signing@govcraft.ai
/// -f <key> <file>` would write, which keeps interop with the wider
/// SSHSIG ecosystem and lets operators audit a SchemaForge `.sig` with
/// `ssh-keygen -Y check-novalidate` if they want a sanity check.
pub struct SshSigner {
    private_key: PrivateKey,
    namespace: String,
    hash_alg: HashAlg,
}

impl SshSigner {
    /// Load an OpenSSH private key from disk. The file must be the
    /// PEM-armored OpenSSH format (`ssh-keygen -t ed25519` output);
    /// encrypted keys are rejected.
    pub fn from_openssh_file(path: &Path) -> Result<Self, SigningError> {
        let key = PrivateKey::read_openssh_file(path).map_err(|e| SigningError::InvalidKey {
            name: path.display().to_string(),
            message: format!("OpenSSH key load failed: {e}"),
        })?;
        if key.is_encrypted() {
            return Err(SigningError::InvalidKey {
                name: path.display().to_string(),
                message: "OpenSSH key is encrypted; decrypt it before signing".into(),
            });
        }
        Ok(Self {
            private_key: key,
            namespace: SSHSIG_NAMESPACE.to_string(),
            hash_alg: HashAlg::Sha512,
        })
    }

    /// Build from an in-memory `PrivateKey`. Used by tests; in
    /// production [`Self::from_openssh_file`] is the usual entry point.
    pub fn from_private_key(private_key: PrivateKey) -> Self {
        Self {
            private_key,
            namespace: SSHSIG_NAMESPACE.to_string(),
            hash_alg: HashAlg::Sha512,
        }
    }

    /// OpenSSH-format public key line (`ssh-ed25519 AAAA... comment`),
    /// suitable for pasting into an `allowed_signers` trust file.
    pub fn public_key_openssh(&self) -> Result<String, SigningError> {
        self.private_key
            .public_key()
            .to_openssh()
            .map_err(|e| SigningError::Other(format!("public key serialise failed: {e}")))
    }

    /// Comment baked into the OpenSSH key (often the operator's email
    /// or `user@host`). Used as a default principal hint by the CLI.
    pub fn comment(&self) -> &str {
        self.private_key.comment()
    }

    /// Compose a ready-to-paste `allowed_signers` line: principal,
    /// space, public-key line. The principal defaults to the key
    /// comment so the operator can copy directly into the trust file.
    pub fn allowed_signers_line(&self, principal: Option<&str>) -> Result<String, SigningError> {
        let principal = principal
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(self.comment()).filter(|c| !c.trim().is_empty()))
            .unwrap_or("schemaforge-operator");
        let pubkey = self.public_key_openssh()?;
        Ok(format!("{principal} {pubkey}"))
    }

    /// Produce a PEM-armored SSHSIG over `message`. Returned bytes are
    /// ASCII text, ready to write to `<file>.sig`.
    pub fn sign_pem(&self, message: &[u8]) -> Result<String, SigningError> {
        let sig = SshSig::sign(&self.private_key, &self.namespace, self.hash_alg, message)
            .map_err(|e| SigningError::SignFailed {
                message: format!("ssh sign failed: {e}"),
            })?;
        sig.to_pem(LineEnding::LF)
            .map_err(|e| SigningError::SignFailed {
                message: format!("ssh signature PEM encode failed: {e}"),
            })
    }
}

impl DirectorySigner for SshSigner {
    fn sign_to_sig_bytes(&self, message: &[u8]) -> Result<Vec<u8>, SigningError> {
        Ok(self.sign_pem(message)?.into_bytes())
    }

    fn kind(&self) -> &'static str {
        "ssh-allowed-signers"
    }
}

/// Cosign-keyless signer that delegates to the operator's `cosign`
/// binary via `cosign sign-blob --bundle …`. We deliberately do not
/// reimplement the OIDC + Fulcio + Rekor protocol — `cosign` is the
/// canonical CLI for that flow, ships in every Sigstore-enabled CI
/// environment, and tracks Fulcio API changes upstream.
///
/// Each [`Self::sign_to_sig_bytes`] call writes the message to a temp
/// file, invokes `cosign sign-blob --yes --bundle <out>.json
/// <message>`, then reads the produced Sigstore Bundle and returns it
/// verbatim as the `.sig` payload. The resulting `.sig` is a
/// self-contained Sigstore Bundle (mediaType
/// `application/vnd.dev.sigstore.bundle.v0.3+json`) that
/// [`crate::verifiers::cosign::CosignKeylessVerifier`] accepts.
///
/// One `cosign sign-blob` invocation per file means one OIDC round
/// trip per file. For large schema directories that's noticeable but
/// not slow; the alternative — reusing a single token — would require
/// reimplementing the Fulcio cert exchange in-process.
pub struct CosignKeylessSigner {
    /// `cosign` binary name or path. Defaults to `cosign` (resolved
    /// via `PATH`).
    cosign_bin: String,
}

impl CosignKeylessSigner {
    /// Build the signer and verify that the configured `cosign`
    /// binary is on `PATH` (or at the supplied absolute path) and
    /// reports a version. The version probe runs once at signer
    /// construction so a missing binary surfaces *before* the first
    /// signing attempt rather than after partial directory work.
    pub fn try_new(cosign_bin: impl Into<String>) -> Result<Self, SigningError> {
        let cosign_bin = cosign_bin.into();
        let probe = std::process::Command::new(&cosign_bin)
            .arg("version")
            .output()
            .map_err(|source| SigningError::InvalidKey {
                name: cosign_bin.clone(),
                message: format!("could not run `{cosign_bin} version`: {source}"),
            })?;
        if !probe.status.success() {
            return Err(SigningError::InvalidKey {
                name: cosign_bin,
                message: format!(
                    "`cosign version` failed (status {:?}): {}",
                    probe.status,
                    String::from_utf8_lossy(&probe.stderr),
                ),
            });
        }
        Ok(Self { cosign_bin })
    }

    /// Borrow the binary name this signer is configured with.
    pub fn cosign_bin(&self) -> &str {
        &self.cosign_bin
    }
}

impl DirectorySigner for CosignKeylessSigner {
    fn sign_to_sig_bytes(&self, message: &[u8]) -> Result<Vec<u8>, SigningError> {
        let temp_dir = tempfile::tempdir().map_err(|source| SigningError::Io {
            path: PathBuf::from("/tmp"),
            source,
        })?;
        let input_path = temp_dir.path().join("blob.bin");
        let bundle_path = temp_dir.path().join("blob.sig");

        std::fs::write(&input_path, message).map_err(|source| SigningError::Io {
            path: input_path.clone(),
            source,
        })?;

        let output = std::process::Command::new(&self.cosign_bin)
            .arg("sign-blob")
            .arg("--yes")
            .arg("--bundle")
            .arg(&bundle_path)
            .arg(&input_path)
            .output()
            .map_err(|source| SigningError::SignFailed {
                message: format!(
                    "spawning `{} sign-blob` failed: {source}",
                    self.cosign_bin,
                ),
            })?;

        if !output.status.success() {
            return Err(SigningError::SignFailed {
                message: format!(
                    "`{} sign-blob` exited with status {:?}: {}",
                    self.cosign_bin,
                    output.status,
                    String::from_utf8_lossy(&output.stderr),
                ),
            });
        }

        std::fs::read(&bundle_path).map_err(|source| SigningError::Io {
            path: bundle_path,
            source,
        })
    }

    fn kind(&self) -> &'static str {
        "cosign-keyless"
    }
}

/// Outcome of signing one schema directory. Useful both for the
/// `schemaforge sign` CLI report and as a tested return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignReport {
    /// Manifest written to disk.
    pub manifest_path: PathBuf,

    /// Signature file written for the manifest.
    pub manifest_signature_path: PathBuf,

    /// One row per signed schema file: the schema path and the
    /// `.sig` path beside it.
    pub signed: Vec<SignedFile>,
}

/// One signed schema file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedFile {
    pub schema_path: PathBuf,
    pub signature_path: PathBuf,
}

/// Sign every file in `files`, producing `<file>.sig` next to each
/// one, then build, write, and sign a manifest at
/// `<manifest_dir>/schemas.manifest.toml`. Existing signature files
/// at those paths are overwritten — this is a re-signing operation
/// by design.
///
/// Generic over [`DirectorySigner`] so it equally drives the Ed25519
/// and SSH signers without conditional logic at the call site.
pub fn sign_directory<S: DirectorySigner>(
    manifest_dir: &Path,
    files: &[PathBuf],
    signer: &S,
    manifest_filename: Option<&str>,
) -> Result<SignReport, SigningError> {
    let mut signed = Vec::with_capacity(files.len());

    for file in files {
        let bytes = std::fs::read(file).map_err(|source| SigningError::Io {
            path: file.clone(),
            source,
        })?;
        let sig_bytes = signer.sign_to_sig_bytes(&bytes)?;
        let sig_path = signature_path_for(file);
        std::fs::write(&sig_path, sig_bytes).map_err(|source| SigningError::Io {
            path: sig_path.clone(),
            source,
        })?;
        signed.push(SignedFile {
            schema_path: file.clone(),
            signature_path: sig_path,
        });
    }

    let manifest = Manifest::build(manifest_dir, files)?;
    let manifest_bytes = manifest.to_toml_bytes()?;
    let manifest_path = manifest_dir.join(manifest_filename.unwrap_or(MANIFEST_FILE_NAME));
    std::fs::write(&manifest_path, &manifest_bytes).map_err(|source| SigningError::Io {
        path: manifest_path.clone(),
        source,
    })?;

    let manifest_sig_bytes = signer.sign_to_sig_bytes(&manifest_bytes)?;
    let manifest_sig_path = signature_path_for(&manifest_path);
    std::fs::write(&manifest_sig_path, manifest_sig_bytes).map_err(|source| SigningError::Io {
        path: manifest_sig_path.clone(),
        source,
    })?;

    Ok(SignReport {
        manifest_path,
        manifest_signature_path: manifest_sig_path,
        signed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixed_signer() -> Ed25519Signer {
        Ed25519Signer::from_seed_bytes(&[0x42; 32])
    }

    fn test_ssh_signer() -> SshSigner {
        let kp = ssh_key::private::Ed25519Keypair::from_seed(&[0x33; 32]);
        let key = PrivateKey::new(kp.into(), "test-key").unwrap();
        SshSigner::from_private_key(key)
    }

    #[test]
    fn sign_directory_writes_sig_and_manifest() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.schema");
        let b = dir.path().join("b.schema");
        std::fs::write(&a, b"schema A {}").unwrap();
        std::fs::write(&b, b"schema B {}").unwrap();
        let signer = fixed_signer();

        let report = sign_directory(dir.path(), &[a.clone(), b.clone()], &signer, None).unwrap();
        assert_eq!(report.signed.len(), 2);
        assert!(report.signed.iter().all(|s| s.signature_path.exists()));
        assert!(report.manifest_path.exists());
        assert!(report.manifest_signature_path.exists());

        // Manifest content should be parseable and pin both files.
        let m = Manifest::read_from(&report.manifest_path).unwrap();
        assert_eq!(m.entries.len(), 2);
        assert!(m.entries.iter().any(|e| e.path == "a.schema"));
        assert!(m.entries.iter().any(|e| e.path == "b.schema"));
    }

    #[test]
    fn sign_directory_with_ssh_writes_pem_armored_sigs() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.schema");
        std::fs::write(&a, b"schema A {}").unwrap();
        let signer = test_ssh_signer();

        let report = sign_directory(dir.path(), std::slice::from_ref(&a), &signer, None).unwrap();
        let sig_text = std::fs::read_to_string(&report.signed[0].signature_path).unwrap();
        assert!(sig_text.starts_with("-----BEGIN SSH SIGNATURE-----"));
        let manifest_sig = std::fs::read_to_string(&report.manifest_signature_path).unwrap();
        assert!(manifest_sig.starts_with("-----BEGIN SSH SIGNATURE-----"));
    }

    #[test]
    fn ssh_signer_advertises_pubkey_for_allowed_signers() {
        let signer = test_ssh_signer();
        let line = signer.allowed_signers_line(Some("ops@example.com")).unwrap();
        assert!(line.starts_with("ops@example.com ssh-ed25519 "));
    }

    #[test]
    fn ssh_signer_falls_back_to_comment_as_principal() {
        let signer = test_ssh_signer();
        let line = signer.allowed_signers_line(None).unwrap();
        // test_ssh_signer uses comment "test-key".
        assert!(line.starts_with("test-key ssh-ed25519 "));
    }

    #[test]
    fn public_key_b64_raw_round_trips_through_verifier() {
        let signer = fixed_signer();
        let pubkey_b64 = signer.public_key_b64_raw();
        let v = crate::verifiers::ed25519::Ed25519Verifier::from_b64("t", &pubkey_b64).unwrap();
        let bytes = b"hello";
        let sig = signer.sign_b64(bytes);
        let id = crate::verifier::SchemaVerifier::verify(
            &v,
            std::path::Path::new("inline"),
            bytes,
            sig.as_bytes(),
            None,
        )
        .unwrap();
        assert_eq!(id.kind, "ed25519");
    }

    #[test]
    fn public_key_b64_spki_round_trips_through_verifier() {
        let signer = fixed_signer();
        let pubkey_b64 = signer.public_key_b64_spki().unwrap();
        let v = crate::verifiers::ed25519::Ed25519Verifier::from_b64("t", &pubkey_b64).unwrap();
        let bytes = b"hello";
        let sig = signer.sign_b64(bytes);
        crate::verifier::SchemaVerifier::verify(
            &v,
            std::path::Path::new("inline"),
            bytes,
            sig.as_bytes(),
            None,
        )
        .unwrap();
    }

    #[test]
    fn keypair_round_trips_through_pem() {
        let dir = tempdir().unwrap();
        let key_path = dir.path().join("k.pem");
        let signer = fixed_signer();
        signer.write_keypair(&key_path).unwrap();
        let loaded = Ed25519Signer::from_pem_file(&key_path).unwrap();
        // Both signers produce the same signature for the same input —
        // proves the PEM round-trip preserved the secret scalar.
        assert_eq!(signer.sign_b64(b"x"), loaded.sign_b64(b"x"));
    }

    #[test]
    fn ssh_signer_round_trips_through_verifier() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("u.schema");
        std::fs::write(&f, b"schema U {}").unwrap();
        let signer = test_ssh_signer();
        sign_directory(dir.path(), std::slice::from_ref(&f), &signer, None).unwrap();

        // Build an allowed_signers verifier from the signer's pubkey,
        // then verify the artefacts produced just above.
        let allowed = signer.allowed_signers_line(Some("test-principal")).unwrap();
        let v = crate::verifiers::ssh::SshAllowedSignersVerifier::from_text("ops", &allowed)
            .unwrap();
        let sig_bytes = std::fs::read(crate::verifiers::ed25519::signature_path_for(&f)).unwrap();
        let file_bytes = std::fs::read(&f).unwrap();
        let id = crate::verifier::SchemaVerifier::verify(
            &v,
            &f,
            &file_bytes,
            &sig_bytes,
            None,
        )
        .unwrap();
        assert_eq!(id.kind, "ssh-allowed-signers");
        assert!(id.name.contains("test-principal"));
    }
}
