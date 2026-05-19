//! Raw-ed25519 verifier.
//!
//! Accepts a public key in any of three encodings:
//!
//! 1. Raw 32-byte key, base64-encoded (`AAAA...` — 44 chars with
//!    padding).
//! 2. SubjectPublicKeyInfo (SPKI) DER blob, base64-encoded — what
//!    `openssl pkey -pubout -outform DER` produces; the leading bytes
//!    encode the Ed25519 OID `1.3.101.112`.
//! 3. PEM file with `-----BEGIN PUBLIC KEY-----` markers — what
//!    `openssl pkey -pubout` (default text mode) or `ssh-keygen -e -m
//!    PKCS8` produces.
//!
//! Signatures are raw 64-byte Ed25519 signatures, base64-encoded. This
//! matches the cosign convention for `.sig` files so operators who
//! already think in cosign terms have one fewer surprise.

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::pkcs8::DecodePublicKey;
use ed25519_dalek::{Signature, Verifier, VerifyingKey, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH};

use crate::error::{SigningError, VerifyError};
use crate::verifier::{SchemaVerifier, VerifiedIdentity};

/// Verifier driven by a single ed25519 public key.
#[derive(Debug)]
pub struct Ed25519Verifier {
    name: String,
    key: VerifyingKey,
}

impl Ed25519Verifier {
    /// Build from a base64-encoded public key.
    ///
    /// The decoder tries the raw-32-byte form first (fastest, what
    /// most operators have when they generate keys via this crate),
    /// then falls back to SubjectPublicKeyInfo DER. Both forms are
    /// rejected with a clear error.
    pub fn from_b64(name: &str, b64: &str) -> Result<Self, SigningError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| SigningError::InvalidKey {
                name: name.to_string(),
                message: format!("base64 decode failed: {e}"),
            })?;

        let key = if bytes.len() == PUBLIC_KEY_LENGTH {
            let mut buf = [0u8; PUBLIC_KEY_LENGTH];
            buf.copy_from_slice(&bytes);
            VerifyingKey::from_bytes(&buf).map_err(|e| SigningError::InvalidKey {
                name: name.to_string(),
                message: format!("not a valid ed25519 point: {e}"),
            })?
        } else {
            // Try SPKI DER.
            VerifyingKey::from_public_key_der(&bytes).map_err(|e| SigningError::InvalidKey {
                name: name.to_string(),
                message: format!(
                    "key is neither raw 32-byte ed25519 (got {} bytes) nor valid SPKI DER: {e}",
                    bytes.len()
                ),
            })?
        };

        Ok(Self {
            name: name.to_string(),
            key,
        })
    }

    /// Build from a PEM-encoded SubjectPublicKeyInfo on disk.
    pub fn from_pem_file(name: &str, path: &Path) -> Result<Self, SigningError> {
        let pem = std::fs::read_to_string(path).map_err(|source| SigningError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let key = VerifyingKey::from_public_key_pem(&pem).map_err(|e| SigningError::InvalidKey {
            name: name.to_string(),
            message: format!("PEM parse failed: {e}"),
        })?;
        Ok(Self {
            name: name.to_string(),
            key,
        })
    }

    /// Expose the verifying key (useful for tests).
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.key
    }
}

impl SchemaVerifier for Ed25519Verifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "ed25519"
    }

    fn verify(
        &self,
        file_path: &Path,
        file_bytes: &[u8],
        signature: &[u8],
        _cert: Option<&[u8]>,
    ) -> Result<VerifiedIdentity, VerifyError> {
        let sig_bytes = decode_signature(file_path, signature)?;
        let sig = Signature::from_bytes(&sig_bytes);
        self.key
            .verify(file_bytes, &sig)
            .map(|()| VerifiedIdentity {
                kind: self.kind(),
                name: self.name.clone(),
            })
            .map_err(|e| VerifyError::UntrustedSigner {
                path: file_path.to_path_buf(),
                reason: format!("ed25519 verify failed against '{}': {e}", self.name),
            })
    }
}

/// Decode the raw 64 signature bytes from a base64 string. Accepts the
/// signature with or without trailing whitespace — sig files written
/// by `schemaforge sign` add a newline, and tooling that strips it
/// still works.
fn decode_signature(file_path: &Path, signature: &[u8]) -> Result<[u8; SIGNATURE_LENGTH], VerifyError> {
    let text = std::str::from_utf8(signature).map_err(|e| VerifyError::MalformedSignature {
        path: file_path.to_path_buf(),
        message: format!("signature file is not valid UTF-8: {e}"),
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| VerifyError::MalformedSignature {
            path: file_path.to_path_buf(),
            message: format!("signature base64 decode failed: {e}"),
        })?;
    if bytes.len() != SIGNATURE_LENGTH {
        return Err(VerifyError::MalformedSignature {
            path: file_path.to_path_buf(),
            message: format!(
                "expected {SIGNATURE_LENGTH}-byte ed25519 signature, got {}",
                bytes.len()
            ),
        });
    }
    let mut out = [0u8; SIGNATURE_LENGTH];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Helper used by tests and by [`crate::signer`]. Made `pub(crate)` so
/// the signer can share the encoding step without going through the
/// CLI surface.
pub(crate) fn encode_signature_b64(sig: &Signature) -> String {
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

/// Convenience used by callers that need to know the on-disk signature
/// file path for a given schema. Returns `<file>.sig`.
pub fn signature_path_for(schema: &Path) -> PathBuf {
    let mut s = schema.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn fixed_signing_key() -> SigningKey {
        // Deterministic test key — never use this seed in production.
        // 32 bytes of 0x42 form a valid Ed25519 secret scalar.
        SigningKey::from_bytes(&[0x42; 32])
    }

    #[test]
    fn from_b64_accepts_raw_32_byte_key() {
        let signing = fixed_signing_key();
        let pubkey_b64 =
            base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes());
        let v = Ed25519Verifier::from_b64("t", &pubkey_b64).unwrap();
        assert_eq!(v.name(), "t");
        assert_eq!(v.kind(), "ed25519");
    }

    #[test]
    fn from_b64_accepts_spki_der_key() {
        use ed25519_dalek::pkcs8::EncodePublicKey;
        let signing = fixed_signing_key();
        let der = signing
            .verifying_key()
            .to_public_key_der()
            .unwrap()
            .into_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let v = Ed25519Verifier::from_b64("t", &b64).unwrap();
        assert_eq!(v.kind(), "ed25519");
    }

    #[test]
    fn from_b64_rejects_garbage() {
        let err = Ed25519Verifier::from_b64("t", "***not base64***").unwrap_err();
        assert!(format!("{err}").contains("base64"));
    }

    #[test]
    fn from_b64_rejects_wrong_length() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        let err = Ed25519Verifier::from_b64("t", &b64).unwrap_err();
        assert!(format!("{err}").contains("16 bytes"));
    }

    #[test]
    fn verify_accepts_valid_signature() {
        let signing = fixed_signing_key();
        let v = Ed25519Verifier {
            name: "primary".into(),
            key: signing.verifying_key(),
        };
        let bytes = b"schema Foo { name: text }";
        let sig = signing.sign(bytes);
        let sig_b64 = encode_signature_b64(&sig);

        let id = v
            .verify(Path::new("foo.schema"), bytes, sig_b64.as_bytes(), None)
            .unwrap();
        assert_eq!(id.kind, "ed25519");
        assert_eq!(id.name, "primary");
    }

    #[test]
    fn verify_rejects_tampered_bytes() {
        let signing = fixed_signing_key();
        let v = Ed25519Verifier {
            name: "primary".into(),
            key: signing.verifying_key(),
        };
        let sig = signing.sign(b"original content");
        let sig_b64 = encode_signature_b64(&sig);

        let err = v
            .verify(
                Path::new("foo.schema"),
                b"tampered content",
                sig_b64.as_bytes(),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, VerifyError::UntrustedSigner { .. }));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        // Two distinct keys produce signatures only their respective
        // public keys verify against.
        let signing_a = fixed_signing_key();
        let signing_b = SigningKey::from_bytes(&[0x77; 32]);

        let verifier_using_b_pubkey = Ed25519Verifier {
            name: "b".into(),
            key: signing_b.verifying_key(),
        };

        let bytes = b"some payload";
        let sig_from_a = signing_a.sign(bytes);
        let sig_b64 = encode_signature_b64(&sig_from_a);

        let err = verifier_using_b_pubkey
            .verify(Path::new("foo.schema"), bytes, sig_b64.as_bytes(), None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::UntrustedSigner { .. }));
    }

    #[test]
    fn verify_rejects_malformed_signature() {
        let signing = fixed_signing_key();
        let v = Ed25519Verifier {
            name: "primary".into(),
            key: signing.verifying_key(),
        };
        let err = v
            .verify(Path::new("foo.schema"), b"content", b"~~not base64~~", None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::MalformedSignature { .. }));
    }

    #[test]
    fn signature_path_for_appends_sig() {
        let p = signature_path_for(Path::new("schemas/user.schema"));
        assert_eq!(p, PathBuf::from("schemas/user.schema.sig"));
    }
}
