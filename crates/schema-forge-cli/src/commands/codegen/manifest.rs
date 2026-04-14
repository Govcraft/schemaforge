//! Generated-file manifest (`.schemaforge-manifest.toml`).
//!
//! The manifest records every file that the generator *owns* in an output
//! directory, keyed by generator name (`hooks` / `site`). On regeneration
//! we load the previous manifest, diff it against the new set of owned
//! paths, and prune any file that is no longer claimed. This is how
//! deleting a schema from `schemas/` causes the old `proto/old.proto` and
//! `src/hooks/old/` to disappear.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::CodegenError;

/// Filename written at the root of every managed output directory.
pub const MANIFEST_FILENAME: &str = ".schemaforge-manifest.toml";

/// Highest manifest schema version this build understands.
pub const MANIFEST_VERSION: u32 = 1;

/// One entry in the manifest's `[[owned]]` list. Kept as a wrapper struct
/// so future fields (e.g. content hash) can be added without breaking
/// the TOML format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManifestEntry {
    /// Relative path from the output directory, using forward slashes.
    pub path: String,
}

/// On-disk generator manifest. Round-trips through TOML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Manifest format version. Readers refuse unknown versions.
    pub version: u32,
    /// Generator name — `"hooks"`, `"site"`, etc.
    pub generator: String,
    /// Sorted list of owned files (relative paths).
    #[serde(default)]
    pub owned: Vec<ManifestEntry>,
}

impl Manifest {
    /// Construct an empty manifest for the given generator.
    pub fn new(generator: impl Into<String>) -> Self {
        Self {
            version: MANIFEST_VERSION,
            generator: generator.into(),
            owned: Vec::new(),
        }
    }

    /// Replace `owned` with the given paths, sorted and deduplicated for
    /// deterministic on-disk output.
    pub fn set_owned_paths<I, P>(&mut self, paths: I)
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for p in paths {
            set.insert(path_to_posix(p.as_ref()));
        }
        self.owned = set.into_iter().map(|path| ManifestEntry { path }).collect();
    }

    /// Return the owned paths as a sorted `BTreeSet<PathBuf>`.
    pub fn owned_path_set(&self) -> BTreeSet<PathBuf> {
        self.owned.iter().map(|e| PathBuf::from(&e.path)).collect()
    }

    /// Load the manifest from `out_dir/.schemaforge-manifest.toml`.
    ///
    /// Returns:
    /// - `Ok(Some(manifest))` on success.
    /// - `Ok(None)` if the file does not exist (first run, or legacy tree).
    /// - `Err(_)` on parse errors, generator mismatch, or unsupported version.
    pub fn load(out_dir: &Path, expected_generator: &str) -> Result<Option<Self>, CodegenError> {
        let path = out_dir.join(MANIFEST_FILENAME);
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path).map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
        let manifest: Self =
            toml::from_str(&text).map_err(|e| CodegenError::ManifestParse {
                path: path.clone(),
                message: e.to_string(),
            })?;
        if manifest.version > MANIFEST_VERSION {
            return Err(CodegenError::ManifestVersionUnsupported {
                path,
                found: manifest.version,
                supported: MANIFEST_VERSION,
            });
        }
        if manifest.generator != expected_generator {
            return Err(CodegenError::ManifestGeneratorMismatch {
                path,
                found: manifest.generator,
                expected: expected_generator.to_string(),
            });
        }
        Ok(Some(manifest))
    }

    /// Serialize to TOML and write to `out_dir/.schemaforge-manifest.toml`,
    /// creating the directory if needed.
    pub fn save(&self, out_dir: &Path) -> Result<(), CodegenError> {
        fs::create_dir_all(out_dir).map_err(|source| CodegenError::Io {
            path: out_dir.to_path_buf(),
            source,
        })?;
        let path = out_dir.join(MANIFEST_FILENAME);
        let body = toml::to_string_pretty(self).map_err(|e| CodegenError::ManifestParse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        // Prepend a plain comment header so humans who open the file get
        // a clear "do not edit" signal. The marker module is not used here
        // because the manifest is a single-file concept that predates
        // marker verification.
        let text = format!("# .schemaforge-manifest.toml — do not edit by hand.\n{body}");
        fs::write(&path, text).map_err(|source| CodegenError::Io { path, source })
    }
}

/// Return the set of paths present in `old` but not in `new` — the
/// candidates for orphan pruning.
pub fn orphaned_paths(old: &Manifest, new_set: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    old.owned_path_set()
        .into_iter()
        .filter(|p| !new_set.contains(p))
        .collect()
}

/// Convert a possibly-mixed-separator relative path into a forward-slash
/// POSIX form. Absolute paths are preserved as-is (callers should not
/// pass them, but we don't silently strip components).
fn path_to_posix(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_manifest() -> Manifest {
        let mut m = Manifest::new("hooks");
        m.set_owned_paths(["Cargo.toml", "build.rs", "src/main.rs"]);
        m
    }

    #[test]
    fn set_owned_paths_sorts_and_dedups() {
        let mut m = Manifest::new("hooks");
        m.set_owned_paths(["z.rs", "a.rs", "m.rs", "a.rs"]);
        let paths: Vec<_> = m.owned.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "m.rs", "z.rs"]);
    }

    #[test]
    fn roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let m = sample_manifest();
        m.save(tmp.path()).unwrap();
        let loaded = Manifest::load(tmp.path(), "hooks").unwrap().unwrap();
        assert_eq!(loaded, m);
    }

    #[test]
    fn load_absent_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(Manifest::load(tmp.path(), "hooks").unwrap().is_none());
    }

    #[test]
    fn load_rejects_generator_mismatch() {
        let tmp = TempDir::new().unwrap();
        sample_manifest().save(tmp.path()).unwrap();
        let err = Manifest::load(tmp.path(), "site").unwrap_err();
        assert!(matches!(
            err,
            CodegenError::ManifestGeneratorMismatch { .. }
        ));
    }

    #[test]
    fn load_rejects_unsupported_version() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(MANIFEST_FILENAME);
        fs::write(
            &path,
            "version = 999\ngenerator = \"hooks\"\nowned = []\n",
        )
        .unwrap();
        let err = Manifest::load(tmp.path(), "hooks").unwrap_err();
        assert!(matches!(
            err,
            CodegenError::ManifestVersionUnsupported { found: 999, .. }
        ));
    }

    #[test]
    fn load_rejects_malformed_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(MANIFEST_FILENAME), "not = valid = toml").unwrap();
        let err = Manifest::load(tmp.path(), "hooks").unwrap_err();
        assert!(matches!(err, CodegenError::ManifestParse { .. }));
    }

    #[test]
    fn orphaned_paths_returns_difference() {
        let mut old = Manifest::new("hooks");
        old.set_owned_paths(["a.rs", "b.rs", "c.rs"]);
        let new_set: BTreeSet<PathBuf> =
            ["a.rs", "c.rs", "d.rs"].iter().map(PathBuf::from).collect();
        let orphans = orphaned_paths(&old, &new_set);
        assert_eq!(orphans, vec![PathBuf::from("b.rs")]);
    }

    #[test]
    fn owned_path_set_roundtrips_separators() {
        let mut m = Manifest::new("hooks");
        m.set_owned_paths([PathBuf::from("src").join("main.rs")]);
        let set = m.owned_path_set();
        assert!(set.contains(&PathBuf::from("src/main.rs")));
    }

    #[test]
    fn save_writes_doc_comment_header() {
        let tmp = TempDir::new().unwrap();
        sample_manifest().save(tmp.path()).unwrap();
        let body = fs::read_to_string(tmp.path().join(MANIFEST_FILENAME)).unwrap();
        assert!(body.starts_with("# .schemaforge-manifest.toml"));
    }
}
