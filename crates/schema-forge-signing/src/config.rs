//! Serde types for the `[schema_forge.signing]` section of `config.toml`.
//!
//! The CLI loads `SigningConfig` as part of `SchemaForgeSettings` and
//! turns it into a [`crate::policy::VerifyPolicy`] before any schema is
//! parsed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The `[schema_forge.signing]` section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SigningConfig {
    /// What to do when verification fails.
    #[serde(default)]
    pub mode: SigningMode,

    /// Trust anchors accepted for both per-file signatures and the
    /// directory manifest signature. A file passes if **any** signer's
    /// verifier accepts the signature — an "OR" over the list — so
    /// rotating in a new signer is additive and never breaks an
    /// existing one.
    #[serde(default)]
    pub trusted_signers: Vec<TrustedSigner>,

    /// Optional override for the manifest filename. Defaults to
    /// `schemas.manifest.toml`.
    #[serde(default)]
    pub manifest_filename: Option<String>,

    /// Path to a pre-fetched Sigstore TUF trust-root bundle, used by
    /// the `cosign-keyless` verifier in airgapped environments. When
    /// unset, the cosign verifier uses the public Sigstore production
    /// trust root via the network. Wired up in Phase 4 — accepted here
    /// today so config files written now stay forward-compatible.
    #[serde(default)]
    pub trust_root_bundle: Option<PathBuf>,
}

/// Severity of a verification failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SigningMode {
    /// Skip verification entirely. The runtime emits a one-time WARN
    /// noting that signing is disabled. Default value so the feature
    /// is opt-in for existing deployments.
    #[default]
    Off,

    /// Run all checks; log failures but do not abort. Useful for
    /// migrating an existing deployment that hasn't been signed yet.
    Warn,

    /// Run all checks; any failure aborts the command. Recommended for
    /// production.
    Enforce,
}

impl SigningMode {
    /// `true` when verification work should run at all.
    pub fn is_active(&self) -> bool {
        !matches!(self, SigningMode::Off)
    }

    /// `true` when a verification failure should abort.
    pub fn is_strict(&self) -> bool {
        matches!(self, SigningMode::Enforce)
    }
}

/// One trust anchor.
///
/// `kind` is a serde discriminant. Adding new variants is additive —
/// existing configs keep parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TrustedSigner {
    /// Raw ed25519. The simplest scheme: a 32-byte public key the
    /// operator generated out-of-band (see `schemaforge sign
    /// --ed25519-key`).
    Ed25519 {
        /// Human label, used in verification reports and audit logs.
        name: String,

        /// Public key as base64. Either the raw 32-byte ed25519 public
        /// key or a SubjectPublicKeyInfo (SPKI) DER blob — both
        /// recognized by [`crate::verifiers::ed25519::Ed25519Verifier`].
        ///
        /// Operators who already have a PEM-encoded public key on disk
        /// can supply its path via [`Self::Ed25519PemFile`] instead.
        public_key_b64: String,
    },

    /// Same as `Ed25519` but the key is loaded from a PEM file at
    /// runtime. Easier for operators who manage keys with openssl /
    /// ssh-keygen and don't want to base64 them into TOML by hand.
    Ed25519PemFile {
        name: String,
        path: PathBuf,
    },

    /// SSH `allowed_signers`-format file. Phase 2 verifier; the variant
    /// is reserved here so config that names it parses cleanly today
    /// (and surfaces an error explaining the verifier isn't enabled yet).
    SshAllowedSigners {
        name: String,
        path: PathBuf,
    },

    /// Sigstore cosign keyless. Phase 3 verifier; reserved variant.
    CosignKeyless {
        /// Optional label.
        #[serde(default)]
        name: Option<String>,

        /// Expected OIDC issuer URL (e.g.,
        /// `https://token.actions.githubusercontent.com`).
        issuer: String,

        /// Glob pattern the OIDC subject must match. For GitHub
        /// Actions this is typically
        /// `https://github.com/OWNER/REPO/.github/workflows/*.yml@refs/tags/v*`.
        subject_pattern: String,
    },
}

impl TrustedSigner {
    /// Stable label used in verification reports.
    pub fn name(&self) -> &str {
        match self {
            TrustedSigner::Ed25519 { name, .. }
            | TrustedSigner::Ed25519PemFile { name, .. }
            | TrustedSigner::SshAllowedSigners { name, .. } => name,
            TrustedSigner::CosignKeyless { name, .. } => name.as_deref().unwrap_or("cosign-keyless"),
        }
    }

    /// Stable kind discriminant, suitable for logging and tests.
    pub fn kind_str(&self) -> &'static str {
        match self {
            TrustedSigner::Ed25519 { .. } | TrustedSigner::Ed25519PemFile { .. } => "ed25519",
            TrustedSigner::SshAllowedSigners { .. } => "ssh-allowed-signers",
            TrustedSigner::CosignKeyless { .. } => "cosign-keyless",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_off() {
        assert_eq!(SigningMode::default(), SigningMode::Off);
        assert!(!SigningMode::default().is_active());
        assert!(!SigningMode::default().is_strict());
    }

    #[test]
    fn enforce_is_active_and_strict() {
        assert!(SigningMode::Enforce.is_active());
        assert!(SigningMode::Enforce.is_strict());
    }

    #[test]
    fn warn_is_active_but_not_strict() {
        assert!(SigningMode::Warn.is_active());
        assert!(!SigningMode::Warn.is_strict());
    }

    #[test]
    fn ed25519_signer_deserialises() {
        let toml_src = r#"
            kind = "ed25519"
            name = "ops-key"
            public_key_b64 = "MCowBQYDK2VwAyEA000000000000000000000000000000000000000000000="
        "#;
        let signer: TrustedSigner = toml::from_str(toml_src).unwrap();
        match signer {
            TrustedSigner::Ed25519 {
                name,
                public_key_b64,
            } => {
                assert_eq!(name, "ops-key");
                assert!(public_key_b64.starts_with("MCowBQYDK2VwAyEA"));
            }
            other => panic!("expected Ed25519, got {other:?}"),
        }
    }

    #[test]
    fn cosign_keyless_signer_deserialises() {
        let toml_src = r#"
            kind = "cosign-keyless"
            issuer = "https://token.actions.githubusercontent.com"
            subject_pattern = "https://github.com/GovCraft/schemaforge/.github/workflows/release.yml@refs/tags/v*"
        "#;
        let signer: TrustedSigner = toml::from_str(toml_src).unwrap();
        match signer {
            TrustedSigner::CosignKeyless {
                issuer,
                subject_pattern,
                name,
            } => {
                assert!(issuer.contains("githubusercontent.com"));
                assert!(subject_pattern.contains("refs/tags/v*"));
                assert!(name.is_none());
            }
            other => panic!("expected CosignKeyless, got {other:?}"),
        }
    }

    #[test]
    fn full_signing_config_deserialises() {
        let toml_src = r#"
            mode = "enforce"

            [[trusted_signers]]
            kind = "ed25519"
            name = "primary"
            public_key_b64 = "AAAA"

            [[trusted_signers]]
            kind = "ssh-allowed-signers"
            name = "ops-team"
            path = "/etc/schemaforge/allowed_signers"
        "#;
        let cfg: SigningConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(cfg.mode, SigningMode::Enforce);
        assert_eq!(cfg.trusted_signers.len(), 2);
        assert_eq!(cfg.trusted_signers[0].kind_str(), "ed25519");
        assert_eq!(cfg.trusted_signers[1].kind_str(), "ssh-allowed-signers");
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg: SigningConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.mode, SigningMode::Off);
        assert!(cfg.trusted_signers.is_empty());
    }

    #[test]
    fn signer_name_falls_back_for_cosign_without_label() {
        let s = TrustedSigner::CosignKeyless {
            name: None,
            issuer: "x".into(),
            subject_pattern: "y".into(),
        };
        assert_eq!(s.name(), "cosign-keyless");
    }
}
