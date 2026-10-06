//! Errors surfaced by the shared codegen primitives.
//!
//! [`CodegenError`] is the single error type returned by every function in
//! the [`codegen`](super) module. Consumers map it at their command-layer boundary.

use std::path::PathBuf;

use super::sentinel::SentinelKind;

/// Failure modes for shared codegen primitives (manifest, marker, sentinel,
/// write plan). Each variant carries enough context for a user-facing error
/// message without forcing callers to re-construct the message themselves.
#[derive(Debug)]
pub enum CodegenError {
    /// Filesystem error against a specific path.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A file tracked by the generator exists on disk but is missing the
    /// `@generated` marker header, so we refuse to overwrite it.
    MarkerMissing { path: PathBuf },

    /// The output directory is non-empty and contains no schemaforge
    /// sentinel — we won't write into an unfamiliar tree.
    ForeignDirectory { path: PathBuf },

    /// A sentinel was found, but it belongs to a different generator (e.g.
    /// the `site` generator pointing at a hooks project).
    SentinelKindMismatch {
        path: PathBuf,
        found: SentinelKind,
        expected: SentinelKind,
    },

    /// An existing manifest declares a different generator than the one
    /// currently running.
    ManifestGeneratorMismatch {
        path: PathBuf,
        found: String,
        expected: String,
    },

    /// The on-disk manifest format version is newer than what this tool
    /// supports.
    ManifestVersionUnsupported {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    /// Manifest TOML failed to parse or serialize.
    ManifestParse { path: PathBuf, message: String },
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io error for {}: {source}", path.display()),
            Self::MarkerMissing { path } => write!(f, "refusing to overwrite {}: file exists but is missing the `@generated` marker. Delete it or move it out of the generated tree and re-run.", path.display()),
            Self::ForeignDirectory { path } => write!(f, "refusing to write into non-empty directory {}: no schemaforge sentinel found. Use an empty directory or pass --force-init.", path.display()),
            Self::SentinelKindMismatch { path, found, expected } => write!(f, "directory {} is managed by a different schemaforge generator (found {found:?}, expected {expected:?})", path.display()),
            Self::ManifestGeneratorMismatch { path, found, expected } => write!(f, "manifest at {} declares generator `{found}`, expected `{expected}`", path.display()),
            Self::ManifestVersionUnsupported { path, found, supported } => write!(f, "manifest at {} declares version {found}, this tool supports up to {supported}", path.display()),
            Self::ManifestParse { path, message } => write!(f, "manifest at {}: {message}", path.display()),
        }
    }
}
impl std::error::Error for CodegenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
