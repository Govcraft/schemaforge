//! Sentinel files marking a directory as schemaforge-managed.
//!
//! On first generation a zero-byte file (e.g. `.schemaforge-hooks`) is
//! written to the root of the output directory. On subsequent runs, the
//! generator refuses to touch any non-empty directory that does *not*
//! contain its sentinel, so `schema-forge hooks generate -o /` cannot
//! nuke an unrelated tree.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::CodegenError;

/// Which schemaforge generator owns a directory. The filename on disk is
/// `.schemaforge-<kind>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentinelKind {
    /// `hooks generate` — scaffolds a gRPC hook service.
    Hooks,
    /// `site generate` — scaffolds a React front-end.
    Site,
}

impl SentinelKind {
    /// The on-disk filename for this sentinel kind.
    pub const fn filename(self) -> &'static str {
        match self {
            Self::Hooks => ".schemaforge-hooks",
            Self::Site => ".schemaforge-site",
        }
    }

    /// All known sentinel kinds — used to detect a wrong-kind sentinel.
    const fn all() -> &'static [Self] {
        &[Self::Hooks, Self::Site]
    }
}

/// Observed state of an output directory with respect to a specific
/// sentinel kind.
#[derive(Debug, PartialEq, Eq)]
pub enum SentinelState {
    /// Directory does not exist, or exists and is empty.
    Empty,
    /// The expected sentinel is present — directory is managed by us.
    Managed,
    /// Another schemaforge sentinel is present. The generator should
    /// refuse to proceed rather than corrupt the other tool's state.
    WrongKind { found: SentinelKind },
    /// Directory is non-empty and contains no schemaforge sentinel of
    /// any kind.
    Foreign,
}

/// Inspect `dir` and classify it relative to the expected sentinel kind.
/// Returns [`CodegenError::Io`] only on unexpected filesystem errors
/// (e.g. permission denied); a missing directory is [`SentinelState::Empty`].
pub fn check_sentinel(dir: &Path, expected: SentinelKind) -> Result<SentinelState, CodegenError> {
    if !dir.exists() {
        return Ok(SentinelState::Empty);
    }
    // Check for expected first — fast happy path.
    if dir.join(expected.filename()).exists() {
        return Ok(SentinelState::Managed);
    }
    // Scan for a wrong-kind sentinel.
    for kind in SentinelKind::all() {
        if *kind != expected && dir.join(kind.filename()).exists() {
            return Ok(SentinelState::WrongKind { found: *kind });
        }
    }
    // No sentinel found — empty dir is fine, anything else is foreign.
    let mut iter = fs::read_dir(dir).map_err(|source| CodegenError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    if iter.next().is_none() {
        Ok(SentinelState::Empty)
    } else {
        Ok(SentinelState::Foreign)
    }
}

/// Write an empty sentinel file at `dir/<kind>.filename()`. Creates the
/// directory if it does not exist. No-op if the sentinel already exists.
pub fn ensure_sentinel(dir: &Path, kind: SentinelKind) -> Result<PathBuf, CodegenError> {
    fs::create_dir_all(dir).map_err(|source| CodegenError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(kind.filename());
    if !path.exists() {
        fs::write(&path, b"").map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn nonexistent_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("nope");
        assert_eq!(
            check_sentinel(&dir, SentinelKind::Hooks).unwrap(),
            SentinelState::Empty
        );
    }

    #[test]
    fn empty_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            check_sentinel(tmp.path(), SentinelKind::Hooks).unwrap(),
            SentinelState::Empty
        );
    }

    #[test]
    fn dir_with_matching_sentinel_is_managed() {
        let tmp = TempDir::new().unwrap();
        ensure_sentinel(tmp.path(), SentinelKind::Hooks).unwrap();
        assert_eq!(
            check_sentinel(tmp.path(), SentinelKind::Hooks).unwrap(),
            SentinelState::Managed
        );
    }

    #[test]
    fn dir_with_wrong_sentinel_is_wrong_kind() {
        let tmp = TempDir::new().unwrap();
        ensure_sentinel(tmp.path(), SentinelKind::Site).unwrap();
        assert_eq!(
            check_sentinel(tmp.path(), SentinelKind::Hooks).unwrap(),
            SentinelState::WrongKind {
                found: SentinelKind::Site,
            }
        );
    }

    #[test]
    fn dir_with_files_and_no_sentinel_is_foreign() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("readme.txt"), "hi").unwrap();
        assert_eq!(
            check_sentinel(tmp.path(), SentinelKind::Hooks).unwrap(),
            SentinelState::Foreign
        );
    }

    #[test]
    fn ensure_sentinel_creates_file() {
        let tmp = TempDir::new().unwrap();
        let p = ensure_sentinel(tmp.path(), SentinelKind::Hooks).unwrap();
        assert!(p.exists());
        assert_eq!(fs::read(&p).unwrap().len(), 0);
    }

    #[test]
    fn ensure_sentinel_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        ensure_sentinel(tmp.path(), SentinelKind::Hooks).unwrap();
        ensure_sentinel(tmp.path(), SentinelKind::Hooks).unwrap();
    }

    #[test]
    fn filename_distinguishes_kinds() {
        assert_eq!(SentinelKind::Hooks.filename(), ".schemaforge-hooks");
        assert_eq!(SentinelKind::Site.filename(), ".schemaforge-site");
    }
}
