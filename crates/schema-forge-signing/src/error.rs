use std::path::PathBuf;

/// Errors that surface while *producing* signatures, building manifests,
/// or loading trust policies. These are caller-facing problems with the
/// inputs the operator supplied (missing key, malformed config, IO failure
/// in a sign or load helper) — they never indicate that a verification
/// *outcome* was negative.
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    /// IO failure (file not found, permission denied, etc.).
    #[error("IO error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Manifest TOML did not parse.
    #[error("invalid manifest at {path}: {message}")]
    InvalidManifest { path: PathBuf, message: String },

    /// A key supplied for signing or as a trust anchor could not be
    /// parsed in any of the supported encodings.
    #[error("invalid key '{name}': {message}")]
    InvalidKey { name: String, message: String },

    /// The trust policy itself is malformed (e.g., empty signer list under
    /// `enforce`, conflicting fields). Distinct from a *failed* verification.
    #[error("invalid trust policy: {message}")]
    InvalidPolicy { message: String },

    /// The signer encountered a problem while producing a signature.
    #[error("signing failed: {message}")]
    SignFailed { message: String },

    /// Generic catch-all.
    #[error("{0}")]
    Other(String),
}

/// Errors that indicate a verification *outcome* was negative — the
/// artifact is present but rejected. Verifiers should produce these so
/// callers can distinguish "infrastructure broken" (`SigningError`) from
/// "signature did not satisfy policy" (`VerifyError`).
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// No signature file (`<file>.sig`) was found next to a schema that
    /// the policy requires be signed.
    #[error("no signature found for {path}")]
    MissingSignature { path: PathBuf },

    /// No manifest was found, but the policy requires one.
    #[error("no manifest found at {path}")]
    MissingManifest { path: PathBuf },

    /// A file on disk has no matching entry in the manifest. Defeats
    /// "drop a new .schema in the dir" attacks.
    #[error("file {path} is not listed in the signed manifest")]
    FileNotInManifest { path: PathBuf },

    /// The manifest lists a file that is not present on disk. Defeats
    /// "delete an expected file" attacks.
    #[error("manifest entry {path} is not present on disk")]
    ManifestEntryMissing { path: PathBuf },

    /// SHA-256 of file content does not match what the manifest pins.
    #[error("hash mismatch for {path}: manifest pins {expected}, content hashes to {actual}")]
    HashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    /// The signature did not verify against any trusted signer.
    #[error("signature for {path} is not from a trusted signer: {reason}")]
    UntrustedSigner { path: PathBuf, reason: String },

    /// Signature bytes were malformed for the declared signature scheme.
    #[error("malformed signature for {path}: {message}")]
    MalformedSignature { path: PathBuf, message: String },

    /// IO failure while reading a signature or certificate sidecar.
    #[error("IO error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
