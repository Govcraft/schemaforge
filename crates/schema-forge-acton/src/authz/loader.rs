//! On-disk loader for operator-supplied custom Cedar policies.
//!
//! Custom policies live in `policies/custom/*.cedar`. They are concatenated
//! with the generated policy set before validation, so they can either
//! tighten generated rules (`forbid`) or add new permits not covered by the
//! generator. Each custom policy carries its file path as provenance,
//! surfaced through audit events.

use std::path::{Path, PathBuf};

/// Origin metadata for a single custom policy file.
#[derive(Debug, Clone)]
pub struct CustomPolicySource {
    /// Absolute path to the `.cedar` file as it was loaded.
    pub path: PathBuf,
    /// Raw policy source text.
    pub text: String,
}

/// Errors raised while loading custom policies.
#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    /// The custom-policy directory could not be read.
    #[error("could not read custom policy directory '{path}': {source}")]
    DirRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// A `.cedar` file could not be read.
    #[error("could not read custom policy '{path}': {source}")]
    FileRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Loads every `*.cedar` file directly inside `dir` (non-recursive).
///
/// A non-existent `dir` is treated as an empty list so a fresh deployment
/// can omit the directory entirely.
pub fn load_custom_policies(dir: &Path) -> Result<Vec<CustomPolicySource>, LoaderError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(source) => {
            return Err(LoaderError::DirRead {
                path: dir.display().to_string(),
                source,
            })
        }
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| LoaderError::DirRead {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("cedar") {
            paths.push(path);
        }
    }
    // Deterministic ordering — operators may have stable expectations.
    paths.sort();

    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| LoaderError::FileRead {
            path: path.display().to_string(),
            source,
        })?;
        out.push(CustomPolicySource { path, text });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_dir_returns_empty() {
        let path = std::path::PathBuf::from("/tmp/definitely-no-such-dir-for-policies");
        let out = load_custom_policies(&path).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn loads_cedar_files_in_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        std::fs::write(dir_path.join("zebra.cedar"), "// z").unwrap();
        std::fs::write(dir_path.join("alpha.cedar"), "// a").unwrap();
        std::fs::write(dir_path.join("ignore.txt"), "skip me").unwrap();
        let out = load_custom_policies(dir_path).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].path.ends_with("alpha.cedar"));
        assert!(out[1].path.ends_with("zebra.cedar"));
    }
}
