//! Errors surfaced by the shared codegen primitives.
//!
//! [`CodegenError`] is the single error type returned by every function in
//! the [`codegen`](super) module. It converts into [`CliError`] at the
//! command-layer boundary via [`From`].

use std::path::PathBuf;

use crate::error::CliError;

use super::sentinel::SentinelKind;

/// Failure modes for shared codegen primitives (manifest, marker, sentinel,
/// write plan). Each variant carries enough context for a user-facing error
/// message without forcing callers to re-construct the message themselves.
#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    /// Filesystem error against a specific path.
    #[error("io error for {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A file tracked by the generator exists on disk but is missing the
    /// `@generated` marker header, so we refuse to overwrite it.
    #[error(
        "refusing to overwrite {path}: file exists but is missing the \
         `@generated` marker. Delete it or move it out of the generated \
         tree and re-run."
    )]
    MarkerMissing { path: PathBuf },

    /// The output directory is non-empty and contains no schemaforge
    /// sentinel — we won't write into an unfamiliar tree.
    #[error(
        "refusing to write into non-empty directory {path}: no schemaforge \
         sentinel found. Use an empty directory or pass --force-init."
    )]
    ForeignDirectory { path: PathBuf },

    /// A sentinel was found, but it belongs to a different generator (e.g.
    /// the `site` generator pointing at a hooks project).
    #[error(
        "directory {path} is managed by a different schemaforge generator \
         (found {found:?}, expected {expected:?})"
    )]
    SentinelKindMismatch {
        path: PathBuf,
        found: SentinelKind,
        expected: SentinelKind,
    },

    /// An existing manifest declares a different generator than the one
    /// currently running.
    #[error(
        "manifest at {path} declares generator `{found}`, expected `{expected}`"
    )]
    ManifestGeneratorMismatch {
        path: PathBuf,
        found: String,
        expected: String,
    },

    /// The on-disk manifest format version is newer than what this tool
    /// supports.
    #[error(
        "manifest at {path} declares version {found}, this tool supports \
         up to {supported}"
    )]
    ManifestVersionUnsupported {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    /// Manifest TOML failed to parse or serialize.
    #[error("manifest at {path}: {message}")]
    ManifestParse { path: PathBuf, message: String },
}

impl From<CodegenError> for CliError {
    fn from(err: CodegenError) -> Self {
        match err {
            CodegenError::Io { path, source } => CliError::Io { path, source },
            other => CliError::Config {
                message: other.to_string(),
            },
        }
    }
}
