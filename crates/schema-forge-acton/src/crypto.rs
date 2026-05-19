//! Process-wide rustls crypto provider installation.
//!
//! rustls 0.23 requires exactly one [`CryptoProvider`] to be set as the
//! process default before any [`ClientConfig::builder`] or
//! [`ServerConfig::builder`] call that does not pass a provider
//! explicitly. SchemaForge installs `aws_lc_rs`, which matches
//! acton-service's `crypto-aws-lc-rs` default and — when the workspace
//! `fips` feature is enabled — links against the FIPS-validated
//! `aws-lc-fips-sys` build of AWS-LC.
//!
//! Idempotent: safe to call from `main`, tests, and library entry
//! points. The first install wins; subsequent calls are no-ops.
//!
//! [`CryptoProvider`]: rustls::crypto::CryptoProvider
//! [`ClientConfig::builder`]: rustls::ClientConfig::builder
//! [`ServerConfig::builder`]: rustls::ServerConfig::builder

/// Install `aws_lc_rs` as the process-wide default rustls
/// [`CryptoProvider`]. Returns `true` if this call performed the
/// install, `false` if a provider was already set (including by a
/// previous call to this function).
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
pub fn install_default_crypto_provider() -> bool {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_ok()
}
