//! SSH `allowed_signers` verifier.
//!
//! Implements signature verification against an OpenSSH
//! `allowed_signers`-format trust file. The signature on disk is an
//! `SSH SIGNATURE` PEM blob — exactly what `ssh-keygen -Y sign` emits —
//! so an operator who already runs `ssh-keygen` for code review
//! attestation can sign schemas without learning a new tool.
//!
//! ## File layout
//!
//! ```text
//! schemas/
//!   user.schema
//!   user.schema.sig            # PEM-armored SSH SIGNATURE
//!   ...
//! ```
//!
//! The trust file follows the format documented in `ssh-keygen(1)` under
//! `ALLOWED SIGNERS`:
//!
//! ```text
//! roland@govcraft.ai ssh-ed25519 AAAAC3Nz... roland-laptop
//! ops-team,ops-team@example namespaces="schema-forge-signing@govcraft.ai" ssh-ed25519 AAAAC3Nz... ops-team
//! roland@govcraft.ai valid-after="20260101" valid-before="20271231" ssh-ed25519 AAAAC3Nz... rotated-2026
//! ```
//!
//! Options supported: `cert-authority` (rejected — direct keys only),
//! `namespaces="ns1,ns2,..."`, `valid-after="..."`, `valid-before="..."`.
//! Time values follow OpenSSH's `YYYYMMDD[HHMM[SS]]`
//! (`Z` suffix optional) format and are interpreted as UTC.
//!
//! ## Signature namespace
//!
//! SchemaForge always signs under the namespace [`SSHSIG_NAMESPACE`].
//! Allowed-signers entries restricted to a different set of namespaces
//! never match SchemaForge artefacts, so an SSH key authorised to
//! attest, say, git commits won't accidentally become trusted to sign
//! schemas.

use std::path::Path;
use std::time::SystemTime;

use ssh_key::{PublicKey, SshSig};
use time::format_description::FormatItem;
use time::macros::format_description;
use time::PrimitiveDateTime;

use crate::error::{SigningError, VerifyError};
use crate::verifier::{SchemaVerifier, VerifiedIdentity};

/// Namespace string SchemaForge signs under. Allowed-signer entries
/// must either not restrict namespaces or include this exact value.
pub const SSHSIG_NAMESPACE: &str = "schema-forge-signing@govcraft.ai";

/// One parsed `allowed_signers` entry.
#[derive(Debug, Clone)]
struct AllowedSignerEntry {
    /// Principals declared on this line (comma-separated in source).
    /// Carried into [`VerifiedIdentity::name`] when this entry accepts a
    /// signature so audit logs name the *signer*, not just the trust
    /// file.
    principals: Vec<String>,

    /// Allowed namespaces, or `None` for "any". When `Some`, the
    /// schema-forge namespace must appear in this list.
    namespaces: Option<Vec<String>>,

    /// Lower bound on the current time (unix seconds), inclusive.
    valid_after: Option<i64>,

    /// Upper bound on the current time (unix seconds), exclusive.
    valid_before: Option<i64>,

    /// The trusted public key for this entry.
    public_key: PublicKey,
}

/// Verifier driven by an OpenSSH `allowed_signers` file.
#[derive(Debug)]
pub struct SshAllowedSignersVerifier {
    name: String,
    namespace: String,
    entries: Vec<AllowedSignerEntry>,
}

impl SshAllowedSignersVerifier {
    /// Build from an on-disk `allowed_signers` file.
    pub fn from_file(name: &str, path: &Path) -> Result<Self, SigningError> {
        let text = std::fs::read_to_string(path).map_err(|source| SigningError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_text(name, &text)
    }

    /// Build from an in-memory `allowed_signers` document. Exposed
    /// primarily for tests but useful any time the file content is
    /// already loaded.
    pub fn from_text(name: &str, text: &str) -> Result<Self, SigningError> {
        Self::from_text_with_namespace(name, text, SSHSIG_NAMESPACE)
    }

    /// Same as [`Self::from_text`] but lets callers override the
    /// namespace SchemaForge signs/verifies under. Reserved for future
    /// use; production callers should stick with
    /// [`Self::from_text`] / [`Self::from_file`] so every SchemaForge
    /// deployment shares the same namespace string.
    pub fn from_text_with_namespace(
        name: &str,
        text: &str,
        namespace: &str,
    ) -> Result<Self, SigningError> {
        let mut entries = Vec::new();
        for (lineno, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let entry = parse_allowed_signer_line(trimmed).map_err(|message| {
                SigningError::InvalidPolicy {
                    message: format!(
                        "allowed_signers trust file '{name}' line {}: {message}",
                        lineno + 1
                    ),
                }
            })?;
            entries.push(entry);
        }
        if entries.is_empty() {
            return Err(SigningError::InvalidPolicy {
                message: format!(
                    "allowed_signers trust file '{name}' has no usable entries — \
                     check for blank or comment-only content"
                ),
            });
        }
        Ok(Self {
            name: name.to_string(),
            namespace: namespace.to_string(),
            entries,
        })
    }

    /// Count of trust entries loaded. Useful for tests.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

impl SchemaVerifier for SshAllowedSignersVerifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "ssh-allowed-signers"
    }

    fn verify(
        &self,
        file_path: &Path,
        file_bytes: &[u8],
        signature: &[u8],
        _cert: Option<&[u8]>,
    ) -> Result<VerifiedIdentity, VerifyError> {
        let sshsig = parse_sshsig(file_path, signature)?;

        if sshsig.namespace() != self.namespace {
            return Err(VerifyError::UntrustedSigner {
                path: file_path.to_path_buf(),
                reason: format!(
                    "ssh signature namespace '{}' does not match expected '{}'",
                    sshsig.namespace(),
                    self.namespace
                ),
            });
        }

        let now = unix_seconds_now();
        let sig_pubkey = sshsig.public_key();

        // Look for an entry whose pubkey matches the signature's pubkey
        // *and* whose time/namespace constraints are satisfied. Multiple
        // entries can share a pubkey (e.g., rotating principals), so we
        // try them all before giving up.
        let mut last_reason: Option<String> = None;
        for entry in &self.entries {
            if entry.public_key.key_data() != sig_pubkey {
                continue;
            }
            if let Some(allowed) = &entry.namespaces {
                if !allowed.iter().any(|ns| ns == &self.namespace) {
                    last_reason = Some(format!(
                        "ssh key for principal(s) '{}' is not authorised for namespace '{}'",
                        entry.principals.join(","),
                        self.namespace
                    ));
                    continue;
                }
            }
            if let Some(after) = entry.valid_after {
                if now < after {
                    last_reason = Some(format!(
                        "ssh key for principal(s) '{}' is not yet valid (valid-after)",
                        entry.principals.join(",")
                    ));
                    continue;
                }
            }
            if let Some(before) = entry.valid_before {
                if now >= before {
                    last_reason = Some(format!(
                        "ssh key for principal(s) '{}' has expired (valid-before)",
                        entry.principals.join(",")
                    ));
                    continue;
                }
            }

            // Time and namespace gates passed — do the cryptographic check.
            return entry
                .public_key
                .verify(&self.namespace, file_bytes, &sshsig)
                .map(|()| VerifiedIdentity {
                    kind: "ssh-allowed-signers",
                    name: identity_name_for(&self.name, entry),
                })
                .map_err(|e| VerifyError::UntrustedSigner {
                    path: file_path.to_path_buf(),
                    reason: format!(
                        "ssh signature did not verify against allowed entry '{}': {e}",
                        entry.principals.join(",")
                    ),
                });
        }

        let reason = last_reason.unwrap_or_else(|| {
            "no allowed_signers entry matches this signature's public key".to_string()
        });
        Err(VerifyError::UntrustedSigner {
            path: file_path.to_path_buf(),
            reason,
        })
    }
}

/// Compose a stable identity string from the trust-file label and the
/// principal-list of the entry that accepted the signature.
fn identity_name_for(trust_label: &str, entry: &AllowedSignerEntry) -> String {
    if entry.principals.is_empty() {
        trust_label.to_string()
    } else {
        format!("{}:{}", trust_label, entry.principals.join(","))
    }
}

/// Decode the SSHSIG PEM blob from the on-disk `.sig` content. Accepts
/// the file with or without trailing whitespace.
fn parse_sshsig(file_path: &Path, signature: &[u8]) -> Result<SshSig, VerifyError> {
    let text = std::str::from_utf8(signature).map_err(|e| VerifyError::MalformedSignature {
        path: file_path.to_path_buf(),
        message: format!("ssh signature file is not valid UTF-8: {e}"),
    })?;
    let trimmed = text.trim();
    if !trimmed.starts_with("-----BEGIN SSH SIGNATURE-----") {
        return Err(VerifyError::MalformedSignature {
            path: file_path.to_path_buf(),
            message: "not a PEM-armored SSH signature (missing BEGIN marker)".into(),
        });
    }
    SshSig::from_pem(trimmed.as_bytes()).map_err(|e| VerifyError::MalformedSignature {
        path: file_path.to_path_buf(),
        message: format!("ssh signature PEM decode failed: {e}"),
    })
}

/// Lowest-level entry parser. Lifted into its own function so the
/// configuration layer can supply a precise line number in errors.
fn parse_allowed_signer_line(line: &str) -> Result<AllowedSignerEntry, String> {
    let (principals_text, rest) =
        take_whitespace_separated(line).ok_or_else(|| "missing principals field".to_string())?;
    let principals: Vec<String> = principals_text
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if principals.is_empty() {
        return Err("principals field is empty".into());
    }

    let mut namespaces: Option<Vec<String>> = None;
    let mut valid_after: Option<i64> = None;
    let mut valid_before: Option<i64> = None;
    let mut cert_authority = false;

    let mut cursor = rest.trim_start();
    loop {
        if cursor.is_empty() {
            return Err("missing public key after principals/options".into());
        }
        if is_key_type_prefix(cursor) {
            break;
        }
        let (option_text, after_option) =
            take_option_token(cursor).ok_or_else(|| "malformed options field".to_string())?;
        match parse_option(&option_text)? {
            ParsedOption::CertAuthority => cert_authority = true,
            ParsedOption::Namespaces(ns) => namespaces = Some(ns),
            ParsedOption::ValidAfter(t) => valid_after = Some(t),
            ParsedOption::ValidBefore(t) => valid_before = Some(t),
        }
        // Options are separated from each other (and from the key) by
        // whitespace. We don't recognise the comma-separated short form
        // that authorized_keys uses; allowed_signers is per-line and
        // whitespace-only.
        cursor = after_option.trim_start();
    }

    if cert_authority {
        return Err(
            "cert-authority entries are not supported; configure the leaf public key directly"
                .into(),
        );
    }

    let public_key = PublicKey::from_openssh(cursor)
        .map_err(|e| format!("invalid OpenSSH public key: {e}"))?;

    if let (Some(after), Some(before)) = (valid_after, valid_before) {
        if before <= after {
            return Err(format!(
                "valid-before ({before}) must be strictly greater than valid-after ({after})"
            ));
        }
    }

    Ok(AllowedSignerEntry {
        principals,
        namespaces,
        valid_after,
        valid_before,
        public_key,
    })
}

fn is_key_type_prefix(s: &str) -> bool {
    let lower_start = s
        .split_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    matches!(
        lower_start.as_str(),
        "ssh-ed25519"
            | "ssh-rsa"
            | "ssh-dss"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
            | "sk-ssh-ed25519@openssh.com"
            | "sk-ecdsa-sha2-nistp256@openssh.com"
    )
}

/// Split off the first whitespace-separated token, returning
/// `(token, remainder)`. Does not handle quoted whitespace — used for
/// the principals field, which never contains spaces in real
/// allowed_signers files.
fn take_whitespace_separated(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// Split off the first option token. Options have the form `key=value`
/// or a bare flag like `cert-authority`; quoted values can contain
/// whitespace (e.g., `namespaces="a, b"`).
fn take_option_token(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut buf = String::new();
    let mut i = 0;
    let mut in_quote = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_quote {
            buf.push(c);
            if c == '"' {
                in_quote = false;
            }
        } else if c == '"' {
            buf.push(c);
            in_quote = true;
        } else if c.is_whitespace() {
            break;
        } else {
            buf.push(c);
        }
        i += 1;
    }
    if buf.is_empty() {
        None
    } else {
        Some((buf, &s[i..]))
    }
}

enum ParsedOption {
    CertAuthority,
    Namespaces(Vec<String>),
    ValidAfter(i64),
    ValidBefore(i64),
}

fn parse_option(opt: &str) -> Result<ParsedOption, String> {
    if opt.eq_ignore_ascii_case("cert-authority") {
        return Ok(ParsedOption::CertAuthority);
    }
    let (key, raw_value) = opt
        .split_once('=')
        .ok_or_else(|| format!("unknown option '{opt}'"))?;
    let key_lower = key.trim().to_ascii_lowercase();
    let value = strip_optional_quotes(raw_value.trim());
    match key_lower.as_str() {
        "namespaces" => {
            let namespaces: Vec<String> = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if namespaces.is_empty() {
                return Err("namespaces=\"\" allows no namespaces — refusing".into());
            }
            Ok(ParsedOption::Namespaces(namespaces))
        }
        "valid-after" => Ok(ParsedOption::ValidAfter(parse_openssh_time(value)?)),
        "valid-before" => Ok(ParsedOption::ValidBefore(parse_openssh_time(value)?)),
        other => Err(format!("unsupported allowed_signers option '{other}'")),
    }
}

fn strip_optional_quotes(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// OpenSSH time format: `YYYYMMDD`, `YYYYMMDDHHMM`, or `YYYYMMDDHHMMSS`,
/// optionally suffixed with `Z`. All values are interpreted as UTC.
fn parse_openssh_time(s: &str) -> Result<i64, String> {
    let body = s.strip_suffix('Z').unwrap_or(s);
    let (date_part, time_part) = match body.len() {
        8 => (body, "000000"),
        12 => (&body[..8], &body[8..]),
        14 => (&body[..8], &body[8..]),
        _ => {
            return Err(format!(
                "expected OpenSSH time YYYYMMDD[HHMM[SS]][Z], got '{s}'"
            ))
        }
    };
    let time_padded: String = match time_part.len() {
        4 => format!("{time_part}00"),
        6 => time_part.to_string(),
        _ => return Err(format!("invalid time component '{time_part}' in '{s}'")),
    };
    let combined = format!("{date_part}{time_padded}");
    const FMT: &[FormatItem<'_>] = format_description!(
        "[year][month][day][hour][minute][second]"
    );
    let dt = PrimitiveDateTime::parse(&combined, FMT)
        .map_err(|e| format!("could not parse '{s}' as YYYYMMDDHHMMSS: {e}"))?;
    let secs = dt.assume_utc().unix_timestamp();
    Ok(secs)
}

fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::private::Ed25519Keypair;
    use ssh_key::{HashAlg, LineEnding, PrivateKey};

    /// Deterministic SSH key for tests — 32-byte seed.
    fn test_keypair() -> PrivateKey {
        let kp = Ed25519Keypair::from_seed(&[0x33; 32]);
        PrivateKey::new(kp.into(), "test-key").unwrap()
    }

    fn openssh_pubkey_line(pk: &PrivateKey) -> String {
        pk.public_key().to_openssh().unwrap()
    }

    fn sign_blob(pk: &PrivateKey, msg: &[u8], namespace: &str) -> String {
        SshSig::sign(pk, namespace, HashAlg::Sha512, msg)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap()
    }

    #[test]
    fn parse_simple_entry() {
        let pk = test_keypair();
        let line = format!("roland@govcraft.ai {}", openssh_pubkey_line(&pk));
        let entry = parse_allowed_signer_line(&line).unwrap();
        assert_eq!(entry.principals, vec!["roland@govcraft.ai".to_string()]);
        assert!(entry.namespaces.is_none());
        assert!(entry.valid_after.is_none());
        assert!(entry.valid_before.is_none());
    }

    #[test]
    fn parse_entry_with_namespaces() {
        let pk = test_keypair();
        let line = format!(
            "roland@govcraft.ai namespaces=\"schema-forge-signing@govcraft.ai\" {}",
            openssh_pubkey_line(&pk)
        );
        let entry = parse_allowed_signer_line(&line).unwrap();
        assert_eq!(
            entry.namespaces.unwrap(),
            vec!["schema-forge-signing@govcraft.ai".to_string()]
        );
    }

    #[test]
    fn parse_entry_with_time_bounds() {
        let pk = test_keypair();
        let line = format!(
            "ops valid-after=\"20240101\" valid-before=\"20991231Z\" {}",
            openssh_pubkey_line(&pk)
        );
        let entry = parse_allowed_signer_line(&line).unwrap();
        assert!(entry.valid_after.is_some());
        assert!(entry.valid_before.is_some());
        assert!(entry.valid_before.unwrap() > entry.valid_after.unwrap());
    }

    #[test]
    fn parse_rejects_cert_authority() {
        let pk = test_keypair();
        let line = format!("ops cert-authority {}", openssh_pubkey_line(&pk));
        let err = parse_allowed_signer_line(&line).unwrap_err();
        assert!(err.contains("cert-authority"));
    }

    #[test]
    fn parse_rejects_unknown_option() {
        let pk = test_keypair();
        let line = format!("ops zz-bogus=\"x\" {}", openssh_pubkey_line(&pk));
        let err = parse_allowed_signer_line(&line).unwrap_err();
        assert!(err.contains("zz-bogus"));
    }

    #[test]
    fn parse_rejects_inverted_time_bounds() {
        let pk = test_keypair();
        let line = format!(
            "ops valid-after=\"20990101\" valid-before=\"20240101\" {}",
            openssh_pubkey_line(&pk)
        );
        let err = parse_allowed_signer_line(&line).unwrap_err();
        assert!(err.contains("valid-before"));
    }

    #[test]
    fn from_text_rejects_empty_file() {
        let err = SshAllowedSignersVerifier::from_text("ops", "# only a comment\n").unwrap_err();
        assert!(matches!(err, SigningError::InvalidPolicy { .. }));
    }

    #[test]
    fn time_parser_accepts_all_lengths() {
        // YYYYMMDD
        let t1 = parse_openssh_time("20240101").unwrap();
        // YYYYMMDDHHMM
        let t2 = parse_openssh_time("202401010000").unwrap();
        // YYYYMMDDHHMMSS
        let t3 = parse_openssh_time("20240101000000").unwrap();
        // Z suffix
        let t4 = parse_openssh_time("20240101000000Z").unwrap();
        assert_eq!(t1, t2);
        assert_eq!(t2, t3);
        assert_eq!(t3, t4);
    }

    #[test]
    fn verify_round_trip() {
        let pk = test_keypair();
        let allowed = format!("roland@govcraft.ai {}", openssh_pubkey_line(&pk));
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let msg = b"schema User { name: text }";
        let sig = sign_blob(&pk, msg, SSHSIG_NAMESPACE);
        let id = v
            .verify(Path::new("user.schema"), msg, sig.as_bytes(), None)
            .unwrap();
        assert_eq!(id.kind, "ssh-allowed-signers");
        assert!(id.name.contains("roland@govcraft.ai"));
    }

    #[test]
    fn verify_rejects_tampered_bytes() {
        let pk = test_keypair();
        let allowed = format!("roland@govcraft.ai {}", openssh_pubkey_line(&pk));
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let sig = sign_blob(&pk, b"original", SSHSIG_NAMESPACE);
        let err = v
            .verify(Path::new("u.schema"), b"tampered", sig.as_bytes(), None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::UntrustedSigner { .. }));
    }

    #[test]
    fn verify_rejects_unknown_public_key() {
        let pk = test_keypair();
        let other = PrivateKey::new(Ed25519Keypair::from_seed(&[0x77; 32]).into(), "other").unwrap();
        let allowed = format!("roland@govcraft.ai {}", openssh_pubkey_line(&pk));
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        // Sign with the *other* key — pubkey won't match any entry.
        let sig = sign_blob(&other, b"body", SSHSIG_NAMESPACE);
        let err = v
            .verify(Path::new("u.schema"), b"body", sig.as_bytes(), None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::UntrustedSigner { .. }));
    }

    #[test]
    fn verify_rejects_wrong_namespace() {
        let pk = test_keypair();
        let allowed = format!("roland@govcraft.ai {}", openssh_pubkey_line(&pk));
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let sig = sign_blob(&pk, b"body", "some-other-namespace");
        let err = v
            .verify(Path::new("u.schema"), b"body", sig.as_bytes(), None)
            .unwrap_err();
        match err {
            VerifyError::UntrustedSigner { reason, .. } => {
                assert!(reason.contains("namespace"));
            }
            other => panic!("expected UntrustedSigner, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_signature_under_disallowed_namespace_option() {
        // Entry whitelists a *different* namespace; SchemaForge namespace
        // is not in the list, so the entry must not match.
        let pk = test_keypair();
        let allowed = format!(
            "roland@govcraft.ai namespaces=\"file@openssh.com\" {}",
            openssh_pubkey_line(&pk)
        );
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let sig = sign_blob(&pk, b"body", SSHSIG_NAMESPACE);
        let err = v
            .verify(Path::new("u.schema"), b"body", sig.as_bytes(), None)
            .unwrap_err();
        match err {
            VerifyError::UntrustedSigner { reason, .. } => {
                assert!(reason.contains("not authorised for namespace"));
            }
            other => panic!("expected namespace-restriction failure, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_expired_key() {
        let pk = test_keypair();
        // Window ended decades ago.
        let allowed = format!(
            "roland@govcraft.ai valid-before=\"19991231\" {}",
            openssh_pubkey_line(&pk)
        );
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let sig = sign_blob(&pk, b"body", SSHSIG_NAMESPACE);
        let err = v
            .verify(Path::new("u.schema"), b"body", sig.as_bytes(), None)
            .unwrap_err();
        match err {
            VerifyError::UntrustedSigner { reason, .. } => {
                assert!(reason.contains("expired"));
            }
            other => panic!("expected expired-key failure, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_not_yet_valid_key() {
        let pk = test_keypair();
        // Window starts in the far future.
        let allowed = format!(
            "roland@govcraft.ai valid-after=\"20990101\" {}",
            openssh_pubkey_line(&pk)
        );
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let sig = sign_blob(&pk, b"body", SSHSIG_NAMESPACE);
        let err = v
            .verify(Path::new("u.schema"), b"body", sig.as_bytes(), None)
            .unwrap_err();
        match err {
            VerifyError::UntrustedSigner { reason, .. } => {
                assert!(reason.contains("not yet valid"));
            }
            other => panic!("expected not-yet-valid failure, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_malformed_signature() {
        let pk = test_keypair();
        let allowed = format!("roland@govcraft.ai {}", openssh_pubkey_line(&pk));
        let v = SshAllowedSignersVerifier::from_text("ops", &allowed).unwrap();
        let err = v
            .verify(Path::new("u.schema"), b"body", b"not pem", None)
            .unwrap_err();
        assert!(matches!(err, VerifyError::MalformedSignature { .. }));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let pk = test_keypair();
        let text = format!(
            "# header comment\n\nroland@govcraft.ai {}\n\n# trailing\n",
            openssh_pubkey_line(&pk)
        );
        let v = SshAllowedSignersVerifier::from_text("ops", &text).unwrap();
        assert_eq!(v.entry_count(), 1);
    }

    #[test]
    fn multiple_principals_share_an_entry() {
        let pk = test_keypair();
        let line = format!(
            "alice@gov,bob@gov {}",
            openssh_pubkey_line(&pk)
        );
        let entry = parse_allowed_signer_line(&line).unwrap();
        assert_eq!(entry.principals.len(), 2);
    }
}
