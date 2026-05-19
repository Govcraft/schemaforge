//! Signing helpers used by `schemaforge sign`.
//!
//! The verifier side is the security-critical surface — the signer
//! side just produces correctly-shaped artefacts the verifier will
//! later accept. Anything an operator can do with raw `cosign sign-blob`
//! or `ssh-keygen -Y sign` we want to be able to do here too, with
//! consistent paths and no per-tool muscle memory.

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{Signer, SigningKey};

use crate::error::SigningError;
use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
use crate::verifiers::ed25519::{encode_signature_b64, signature_path_for};

/// One signing key paired with the path layout the verifier expects.
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
pub fn sign_directory(
    manifest_dir: &Path,
    files: &[PathBuf],
    signer: &Ed25519Signer,
    manifest_filename: Option<&str>,
) -> Result<SignReport, SigningError> {
    let mut signed = Vec::with_capacity(files.len());

    for file in files {
        let bytes = std::fs::read(file).map_err(|source| SigningError::Io {
            path: file.clone(),
            source,
        })?;
        let sig_b64 = signer.sign_b64(&bytes);
        let sig_path = signature_path_for(file);
        std::fs::write(&sig_path, sig_b64).map_err(|source| SigningError::Io {
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

    let manifest_sig_b64 = signer.sign_b64(&manifest_bytes);
    let manifest_sig_path = signature_path_for(&manifest_path);
    std::fs::write(&manifest_sig_path, manifest_sig_b64).map_err(|source| SigningError::Io {
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
}
