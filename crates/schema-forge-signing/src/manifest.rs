//! Signed directory manifest.
//!
//! The manifest enumerates every `.schema` file expected to live under a
//! given root directory, pinning each by SHA-256. Pairing per-file
//! signatures with a signed manifest closes the two gaps a per-file
//! scheme alone has:
//!
//! - **Add attack**: drop a new `.schema` plus a forged-but-actually-
//!   well-formed `.sig` from a key the operator doesn't own. The
//!   manifest enumerates expected files; an extra file is rejected.
//! - **Delete attack**: remove a `.schema` (e.g., one that enforced a
//!   tenant boundary). The manifest lists files that must be present;
//!   a missing file is rejected.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{SigningError, VerifyError};
use crate::hash::sha256_hex;

/// Default filename. Operators can override via
/// [`crate::config::SigningConfig::manifest_filename`].
pub const MANIFEST_FILE_NAME: &str = "schemas.manifest.toml";

/// Currently-known manifest schema version. Bumped only on
/// breaking-format changes; the deserializer rejects unknown versions
/// rather than silently misinterpreting them.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// On-disk form of `schemas.manifest.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Format version. Always [`MANIFEST_SCHEMA_VERSION`] for files this
    /// crate writes; reading rejects anything else.
    pub schema_version: u32,

    /// One entry per `.schema` file expected under the manifest's root
    /// directory. Sorted lexicographically by `path` so manifests are
    /// byte-identical across re-signings of the same content.
    pub entries: Vec<ManifestEntry>,
}

/// One row of [`Manifest::entries`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Path relative to the manifest file's directory, with `/`
    /// separators on every platform.
    pub path: String,

    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: String,
}

impl Manifest {
    /// Build a manifest by hashing every file in `files` relative to
    /// `manifest_dir`. Files outside `manifest_dir` are rejected — the
    /// manifest is meaningless if it doesn't pin a coherent root.
    pub fn build(manifest_dir: &Path, files: &[PathBuf]) -> Result<Self, SigningError> {
        let mut entries = Vec::with_capacity(files.len());
        for file in files {
            let rel = relativise(manifest_dir, file)?;
            let bytes = std::fs::read(file).map_err(|source| SigningError::Io {
                path: file.clone(),
                source,
            })?;
            entries.push(ManifestEntry {
                path: rel,
                sha256: sha256_hex(&bytes),
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            entries,
        })
    }

    /// Serialize to canonical TOML bytes. Used both when writing to
    /// disk and when constructing the bytes the signature covers — the
    /// signature is computed over **exactly** what the verifier later
    /// reads back, with no whitespace normalization on either side.
    pub fn to_toml_bytes(&self) -> Result<Vec<u8>, SigningError> {
        toml::to_string_pretty(self)
            .map(String::into_bytes)
            .map_err(|e| SigningError::Other(format!("failed to serialize manifest: {e}")))
    }

    /// Parse the bytes of a manifest file.
    pub fn from_bytes(path: &Path, bytes: &[u8]) -> Result<Self, SigningError> {
        let text = std::str::from_utf8(bytes).map_err(|e| SigningError::InvalidManifest {
            path: path.to_path_buf(),
            message: format!("manifest is not valid UTF-8: {e}"),
        })?;
        let manifest: Manifest =
            toml::from_str(text).map_err(|e| SigningError::InvalidManifest {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(SigningError::InvalidManifest {
                path: path.to_path_buf(),
                message: format!(
                    "unsupported manifest schema_version {} (this build understands {})",
                    manifest.schema_version, MANIFEST_SCHEMA_VERSION
                ),
            });
        }
        Ok(manifest)
    }

    /// Read a manifest from disk. Convenience wrapper that combines IO
    /// with [`Self::from_bytes`].
    pub fn read_from(path: &Path) -> Result<Self, SigningError> {
        let bytes = std::fs::read(path).map_err(|source| SigningError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_bytes(path, &bytes)
    }

    /// Cross-check: every file in `disk_files` (paths relative to
    /// `manifest_dir`) must be listed, and every entry must be present
    /// on disk. Returns the first discrepancy.
    pub fn validate_against_disk(
        &self,
        manifest_dir: &Path,
        disk_files: &[PathBuf],
    ) -> Result<(), VerifyError> {
        let listed: std::collections::HashSet<&str> =
            self.entries.iter().map(|e| e.path.as_str()).collect();

        for file in disk_files {
            let rel = relativise_for_verify(manifest_dir, file)?;
            if !listed.contains(rel.as_str()) {
                return Err(VerifyError::FileNotInManifest {
                    path: file.clone(),
                });
            }
        }

        let on_disk: std::collections::HashSet<String> = disk_files
            .iter()
            .filter_map(|f| relativise_for_verify(manifest_dir, f).ok())
            .collect();

        for entry in &self.entries {
            if !on_disk.contains(&entry.path) {
                return Err(VerifyError::ManifestEntryMissing {
                    path: manifest_dir.join(&entry.path),
                });
            }
        }

        Ok(())
    }

    /// Look up the pinned hash for a file path relative to the
    /// manifest's root directory.
    pub fn pinned_hash(&self, relative_path: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.path == relative_path)
            .map(|e| e.sha256.as_str())
    }
}

/// Express `target` as a `/`-separated string relative to `base`,
/// returning [`SigningError::Other`] when `target` is not actually
/// inside `base`. Used when **building** a manifest, where being
/// outside the root is a programmer/operator error.
fn relativise(base: &Path, target: &Path) -> Result<String, SigningError> {
    relativise_inner(base, target)
        .map_err(|reason| SigningError::Other(format!("cannot relativise {target:?}: {reason}")))
}

/// Same as [`relativise`] but produces a [`VerifyError`] — used when
/// **verifying**, where a file outside the manifest root means the
/// caller fed us mixed inputs.
fn relativise_for_verify(base: &Path, target: &Path) -> Result<String, VerifyError> {
    relativise_inner(base, target).map_err(|_| VerifyError::FileNotInManifest {
        path: target.to_path_buf(),
    })
}

fn relativise_inner(base: &Path, target: &Path) -> Result<String, String> {
    let base = base
        .canonicalize()
        .unwrap_or_else(|_| base.to_path_buf());
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());

    let rel = target
        .strip_prefix(&base)
        .map_err(|_| format!("{target:?} is not under {base:?}"))?;

    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(s) => {
                parts.push(s.to_string_lossy().into_owned());
            }
            _ => {
                return Err(format!("unsupported path component in {rel:?}"));
            }
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(p: &Path, content: &[u8]) {
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn build_pins_each_file_and_sorts() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("b.schema"), b"schema B {}");
        write(&dir.path().join("a.schema"), b"schema A {}");
        let files = vec![dir.path().join("b.schema"), dir.path().join("a.schema")];
        let manifest = Manifest::build(dir.path(), &files).unwrap();
        assert_eq!(manifest.schema_version, MANIFEST_SCHEMA_VERSION);
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(manifest.entries[0].path, "a.schema");
        assert_eq!(manifest.entries[1].path, "b.schema");
        assert_eq!(manifest.entries[0].sha256, sha256_hex(b"schema A {}"));
    }

    #[test]
    fn toml_round_trip_is_byte_stable() {
        let m = Manifest {
            schema_version: 1,
            entries: vec![
                ManifestEntry {
                    path: "a.schema".into(),
                    sha256: "aa".into(),
                },
                ManifestEntry {
                    path: "b.schema".into(),
                    sha256: "bb".into(),
                },
            ],
        };
        let bytes = m.to_toml_bytes().unwrap();
        let parsed = Manifest::from_bytes(Path::new("inline"), &bytes).unwrap();
        assert_eq!(parsed, m);
        // Idempotent: re-serializing the parsed copy yields the same
        // bytes, so a signature over `bytes` will continue to verify.
        assert_eq!(parsed.to_toml_bytes().unwrap(), bytes);
    }

    #[test]
    fn unknown_schema_version_rejected() {
        let m = Manifest {
            schema_version: 9999,
            entries: vec![],
        };
        let bytes = m.to_toml_bytes().unwrap();
        let err = Manifest::from_bytes(Path::new("inline"), &bytes).unwrap_err();
        assert!(format!("{err}").contains("schema_version"));
    }

    #[test]
    fn validate_rejects_extra_file_on_disk() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.schema"), b"A");
        write(&dir.path().join("rogue.schema"), b"R");
        let listed_only = vec![dir.path().join("a.schema")];
        let manifest = Manifest::build(dir.path(), &listed_only).unwrap();

        let disk = vec![dir.path().join("a.schema"), dir.path().join("rogue.schema")];
        let err = manifest
            .validate_against_disk(dir.path(), &disk)
            .unwrap_err();
        assert!(matches!(err, VerifyError::FileNotInManifest { .. }));
    }

    #[test]
    fn validate_rejects_missing_file() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.schema"), b"A");
        write(&dir.path().join("b.schema"), b"B");
        let both = vec![dir.path().join("a.schema"), dir.path().join("b.schema")];
        let manifest = Manifest::build(dir.path(), &both).unwrap();

        std::fs::remove_file(dir.path().join("b.schema")).unwrap();
        let disk = vec![dir.path().join("a.schema")];
        let err = manifest
            .validate_against_disk(dir.path(), &disk)
            .unwrap_err();
        assert!(matches!(err, VerifyError::ManifestEntryMissing { .. }));
    }

    #[test]
    fn validate_accepts_exact_match() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.schema"), b"A");
        write(&dir.path().join("b.schema"), b"B");
        let files = vec![dir.path().join("a.schema"), dir.path().join("b.schema")];
        let manifest = Manifest::build(dir.path(), &files).unwrap();
        manifest
            .validate_against_disk(dir.path(), &files)
            .unwrap();
    }

    #[test]
    fn build_refuses_file_outside_root() {
        let dir = tempdir().unwrap();
        let other = tempdir().unwrap();
        write(&other.path().join("x.schema"), b"X");
        let err = Manifest::build(dir.path(), &[other.path().join("x.schema")]).unwrap_err();
        assert!(format!("{err}").contains("relativise"));
    }
}
