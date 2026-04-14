//! File-plan execution — the heart of the shared codegen subsystem.
//!
//! Callers build a flat `Vec<FilePlan>` describing every file they want to
//! produce, tagged with a [`WriteMode`], and hand the plan to [`write_plan`]
//! (for real writes) or [`check_plan`] (for `--check` dry-runs).
//!
//! `write_plan` guarantees:
//! - New, empty, or previously-managed directories are written to.
//! - Non-empty directories without a sentinel are refused (unless the
//!   caller passes `force_init`).
//! - Every `Owned` file carries a `@generated` marker; re-runs verify the
//!   marker before overwriting.
//! - `Preserve` files are written once, left alone on subsequent runs,
//!   and rewritten only with `force_user_files = true`.
//! - Orphaned `Owned` files from a previous manifest are pruned (and
//!   their parent directories tidied).
//! - A fresh `.schemaforge-manifest.toml` + sentinel are left behind.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::CodegenError;
use super::manifest::{orphaned_paths, Manifest};
use super::marker::Marker;
use super::sentinel::{check_sentinel, ensure_sentinel, SentinelKind, SentinelState};

/// Ownership semantics for a single generated file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// The generator owns this file. It is always overwritten (after
    /// marker verification on re-runs) and tracked in the manifest so
    /// orphans can be pruned.
    Owned,
    /// The generator scaffolds this file once, then leaves it alone.
    /// Users edit it freely. Not tracked in the manifest. Rewritten only
    /// when the caller passes `force_user_files = true`.
    Preserve,
}

/// One entry in a codegen plan.
#[derive(Debug, Clone)]
pub struct FilePlan {
    /// Path relative to the output directory.
    pub relative_path: PathBuf,
    /// File contents *without* a marker header — `write_plan` will
    /// prepend the appropriate marker for `Owned` files.
    pub contents: String,
    /// Ownership mode.
    pub mode: WriteMode,
}

/// Options controlling [`write_plan`] behavior.
#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    /// Generator identifier (e.g. `"hooks"`), embedded in markers and
    /// the manifest.
    pub generator: &'static str,
    /// Which sentinel kind to enforce on the output directory.
    pub sentinel_kind: SentinelKind,
    /// If true, rewrite `Preserve`-mode files even when they already exist.
    pub force_user_files: bool,
    /// If true, write into a non-empty, non-schemaforge directory. This is
    /// the override for the "refuse to write into foreign tree" guard.
    pub force_init: bool,
}

/// Drift report produced by [`check_plan`].
#[derive(Debug, Default)]
pub struct CheckReport {
    /// `Owned` paths whose on-disk content differs from the plan.
    pub differing: Vec<PathBuf>,
    /// `Owned` paths that the plan would write but are not on disk.
    pub missing: Vec<PathBuf>,
    /// Paths present in the previous manifest but absent from the new plan.
    pub orphaned: Vec<PathBuf>,
}

impl CheckReport {
    /// Returns `true` when the generator is idempotent — nothing would
    /// change on a real run.
    pub fn is_clean(&self) -> bool {
        self.differing.is_empty() && self.missing.is_empty() && self.orphaned.is_empty()
    }
}

/// Execute a plan against an on-disk output directory.
pub fn write_plan(
    out_dir: &Path,
    plan: &[FilePlan],
    options: WriteOptions,
) -> Result<(), CodegenError> {
    let marker = Marker::new(options.generator);

    guard_output_directory(out_dir, options.sentinel_kind, options.force_init)?;

    fs::create_dir_all(out_dir).map_err(|source| CodegenError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    // Drop the sentinel *before* we start writing so a later abort still
    // leaves the directory in a schemaforge-managed state.
    ensure_sentinel(out_dir, options.sentinel_kind)?;

    // Load old manifest so we can compute orphans to prune *after*
    // writing the new files. We tolerate a missing manifest (first run).
    let old_manifest = Manifest::load(out_dir, options.generator)?;

    // Write every planned file.
    for entry in plan {
        write_one(out_dir, entry, &marker, options.force_user_files)?;
    }

    // Build + save the new manifest from owned paths only.
    let new_manifest = manifest_from_plan(options.generator, plan);
    let new_set: BTreeSet<PathBuf> = plan
        .iter()
        .filter(|e| matches!(e.mode, WriteMode::Owned))
        .map(|e| e.relative_path.clone())
        .collect();

    // Prune orphans before persisting the new manifest — that way a
    // failed prune leaves the old manifest intact and the operation is
    // retriable.
    if let Some(old) = &old_manifest {
        prune_orphans(out_dir, &orphaned_paths(old, &new_set), &marker)?;
    }

    new_manifest.save(out_dir)?;

    Ok(())
}

/// Dry-run equivalent of [`write_plan`] — reports whether the on-disk
/// state matches what a real run would produce. `Preserve` files are
/// ignored (they are user-owned).
pub fn check_plan(
    out_dir: &Path,
    plan: &[FilePlan],
    options: WriteOptions,
) -> Result<CheckReport, CodegenError> {
    let marker = Marker::new(options.generator);
    let mut report = CheckReport::default();

    for entry in plan {
        if !matches!(entry.mode, WriteMode::Owned) {
            continue;
        }
        let full = out_dir.join(&entry.relative_path);
        let desired = marker.prepend(&entry.relative_path, &entry.contents);
        match fs::read_to_string(&full) {
            Ok(existing) => {
                if existing != desired {
                    report.differing.push(entry.relative_path.clone());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                report.missing.push(entry.relative_path.clone());
            }
            Err(source) => {
                return Err(CodegenError::Io { path: full, source });
            }
        }
    }

    if let Some(old) = Manifest::load(out_dir, options.generator)? {
        let new_set: BTreeSet<PathBuf> = plan
            .iter()
            .filter(|e| matches!(e.mode, WriteMode::Owned))
            .map(|e| e.relative_path.clone())
            .collect();
        report.orphaned = orphaned_paths(&old, &new_set);
    }

    Ok(report)
}

fn manifest_from_plan(generator: &str, plan: &[FilePlan]) -> Manifest {
    let mut manifest = Manifest::new(generator);
    let owned: Vec<PathBuf> = plan
        .iter()
        .filter(|e| matches!(e.mode, WriteMode::Owned))
        .map(|e| e.relative_path.clone())
        .collect();
    manifest.set_owned_paths(owned);
    manifest
}

fn guard_output_directory(
    out_dir: &Path,
    kind: SentinelKind,
    force_init: bool,
) -> Result<(), CodegenError> {
    match check_sentinel(out_dir, kind)? {
        SentinelState::Empty | SentinelState::Managed => Ok(()),
        SentinelState::WrongKind { found } => Err(CodegenError::SentinelKindMismatch {
            path: out_dir.to_path_buf(),
            found,
            expected: kind,
        }),
        SentinelState::Foreign => {
            if force_init {
                Ok(())
            } else {
                Err(CodegenError::ForeignDirectory {
                    path: out_dir.to_path_buf(),
                })
            }
        }
    }
}

fn write_one(
    out_dir: &Path,
    entry: &FilePlan,
    marker: &Marker,
    force_user_files: bool,
) -> Result<(), CodegenError> {
    let full = out_dir.join(&entry.relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|source| CodegenError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    match entry.mode {
        WriteMode::Owned => {
            if full.exists() {
                let existing =
                    fs::read_to_string(&full).map_err(|source| CodegenError::Io {
                        path: full.clone(),
                        source,
                    })?;
                marker.verify(&entry.relative_path, &existing)?;
            }
            let body = marker.prepend(&entry.relative_path, &entry.contents);
            fs::write(&full, body).map_err(|source| CodegenError::Io {
                path: full,
                source,
            })?;
        }
        WriteMode::Preserve => {
            if full.exists() && !force_user_files {
                return Ok(());
            }
            fs::write(&full, &entry.contents).map_err(|source| CodegenError::Io {
                path: full,
                source,
            })?;
        }
    }
    Ok(())
}

fn prune_orphans(
    out_dir: &Path,
    orphans: &[PathBuf],
    marker: &Marker,
) -> Result<(), CodegenError> {
    for relative in orphans {
        let full = out_dir.join(relative);
        if !full.exists() {
            continue;
        }
        let existing = fs::read_to_string(&full).map_err(|source| CodegenError::Io {
            path: full.clone(),
            source,
        })?;
        // Refuse to delete a file that lost its marker — that's a signal
        // the user took ownership and we should not silently destroy work.
        marker.verify(relative, &existing)?;
        fs::remove_file(&full).map_err(|source| CodegenError::Io {
            path: full.clone(),
            source,
        })?;
        prune_empty_parents(out_dir, &full)?;
    }
    Ok(())
}

/// Walk upward from `leaf`'s parent, removing empty directories, stopping
/// as soon as a non-empty directory is hit or `out_dir` is reached.
fn prune_empty_parents(out_dir: &Path, leaf: &Path) -> Result<(), CodegenError> {
    let mut cursor = leaf.parent();
    while let Some(dir) = cursor {
        if dir == out_dir || !dir.starts_with(out_dir) {
            break;
        }
        let mut iter = match fs::read_dir(dir) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(source) => {
                return Err(CodegenError::Io {
                    path: dir.to_path_buf(),
                    source,
                });
            }
        };
        if iter.next().is_some() {
            break;
        }
        fs::remove_dir(dir).map_err(|source| CodegenError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        cursor = dir.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn options() -> WriteOptions {
        WriteOptions {
            generator: "hooks",
            sentinel_kind: SentinelKind::Hooks,
            force_user_files: false,
            force_init: false,
        }
    }

    fn plan_owned(path: &str, contents: &str) -> FilePlan {
        FilePlan {
            relative_path: PathBuf::from(path),
            contents: contents.to_string(),
            mode: WriteMode::Owned,
        }
    }

    fn plan_preserve(path: &str, contents: &str) -> FilePlan {
        FilePlan {
            relative_path: PathBuf::from(path),
            contents: contents.to_string(),
            mode: WriteMode::Preserve,
        }
    }

    #[test]
    fn write_plan_new_dir_happy_path() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![
            plan_owned("Cargo.toml", "[package]\nname = \"x\"\n"),
            plan_owned("src/main.rs", "fn main() {}\n"),
            plan_preserve("src/hooks/user.rs", "// user code\n"),
        ];
        write_plan(tmp.path(), &plan, options()).unwrap();

        assert!(tmp.path().join(".schemaforge-hooks").exists());
        assert!(tmp.path().join(".schemaforge-manifest.toml").exists());
        let cargo = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(cargo.starts_with("# @generated by schema-forge hooks"));
        let main = fs::read_to_string(tmp.path().join("src/main.rs")).unwrap();
        assert!(main.starts_with("// @generated by schema-forge hooks"));
        let user = fs::read_to_string(tmp.path().join("src/hooks/user.rs")).unwrap();
        assert_eq!(user, "// user code\n");
    }

    #[test]
    fn write_plan_preserve_survives_rerun() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_preserve("src/hooks/user.rs", "// original\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();

        fs::write(tmp.path().join("src/hooks/user.rs"), "// edited\n").unwrap();

        write_plan(tmp.path(), &plan, options()).unwrap();
        let after = fs::read_to_string(tmp.path().join("src/hooks/user.rs")).unwrap();
        assert_eq!(after, "// edited\n");
    }

    #[test]
    fn write_plan_preserve_rewritten_with_force_user_files() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_preserve("src/hooks/user.rs", "// original\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();
        fs::write(tmp.path().join("src/hooks/user.rs"), "// edited\n").unwrap();

        let mut opts = options();
        opts.force_user_files = true;
        write_plan(tmp.path(), &plan, opts).unwrap();
        let after = fs::read_to_string(tmp.path().join("src/hooks/user.rs")).unwrap();
        assert_eq!(after, "// original\n");
    }

    #[test]
    fn write_plan_overwrites_owned_with_marker_intact() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();

        // Change the plan contents; re-run should overwrite (marker stays).
        let plan2 = vec![plan_owned("src/main.rs", "fn main() { /* v2 */ }\n")];
        write_plan(tmp.path(), &plan2, options()).unwrap();
        let body = fs::read_to_string(tmp.path().join("src/main.rs")).unwrap();
        assert!(body.contains("v2"));
        assert!(body.starts_with("// @generated"));
    }

    #[test]
    fn write_plan_errors_on_owned_without_marker() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();
        // Corrupt the file: strip marker.
        fs::write(tmp.path().join("src/main.rs"), "fn main() { /* no marker */ }\n").unwrap();
        let err = write_plan(tmp.path(), &plan, options()).unwrap_err();
        assert!(matches!(err, CodegenError::MarkerMissing { .. }));
    }

    #[test]
    fn write_plan_prunes_orphans() {
        let tmp = TempDir::new().unwrap();
        let plan_v1 = vec![
            plan_owned("proto/a.proto", "syntax = \"proto3\";\n"),
            plan_owned("proto/b.proto", "syntax = \"proto3\";\n"),
        ];
        write_plan(tmp.path(), &plan_v1, options()).unwrap();
        assert!(tmp.path().join("proto/b.proto").exists());

        let plan_v2 = vec![plan_owned("proto/a.proto", "syntax = \"proto3\";\n")];
        write_plan(tmp.path(), &plan_v2, options()).unwrap();
        assert!(!tmp.path().join("proto/b.proto").exists());
        assert!(tmp.path().join("proto/a.proto").exists());
    }

    #[test]
    fn write_plan_prune_refuses_hand_edited_orphan() {
        let tmp = TempDir::new().unwrap();
        let plan_v1 = vec![plan_owned("proto/b.proto", "syntax = \"proto3\";\n")];
        write_plan(tmp.path(), &plan_v1, options()).unwrap();
        // User strips marker from the now-to-be-orphaned file.
        fs::write(tmp.path().join("proto/b.proto"), "syntax = \"proto3\";\n").unwrap();

        let plan_v2: Vec<FilePlan> = vec![];
        let err = write_plan(tmp.path(), &plan_v2, options()).unwrap_err();
        assert!(matches!(err, CodegenError::MarkerMissing { .. }));
        // File should NOT have been deleted.
        assert!(tmp.path().join("proto/b.proto").exists());
    }

    #[test]
    fn write_plan_prune_collapses_empty_dirs() {
        let tmp = TempDir::new().unwrap();
        let plan_v1 = vec![plan_owned("proto/nested/x/file.proto", "syntax = \"proto3\";\n")];
        write_plan(tmp.path(), &plan_v1, options()).unwrap();
        let plan_v2: Vec<FilePlan> = vec![];
        write_plan(tmp.path(), &plan_v2, options()).unwrap();
        assert!(!tmp.path().join("proto/nested/x/file.proto").exists());
        assert!(!tmp.path().join("proto/nested/x").exists());
        assert!(!tmp.path().join("proto/nested").exists());
        assert!(!tmp.path().join("proto").exists());
        // out_dir itself is preserved.
        assert!(tmp.path().exists());
    }

    #[test]
    fn write_plan_refuses_foreign_directory() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("unrelated.txt"), "hi").unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        let err = write_plan(tmp.path(), &plan, options()).unwrap_err();
        assert!(matches!(err, CodegenError::ForeignDirectory { .. }));
        // File not written.
        assert!(!tmp.path().join("src/main.rs").exists());
    }

    #[test]
    fn write_plan_force_init_overrides_foreign_check() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("unrelated.txt"), "hi").unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        let mut opts = options();
        opts.force_init = true;
        write_plan(tmp.path(), &plan, opts).unwrap();
        assert!(tmp.path().join("src/main.rs").exists());
        assert!(tmp.path().join("unrelated.txt").exists());
    }

    #[test]
    fn write_plan_refuses_wrong_kind_sentinel() {
        let tmp = TempDir::new().unwrap();
        ensure_sentinel(tmp.path(), SentinelKind::Site).unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        let err = write_plan(tmp.path(), &plan, options()).unwrap_err();
        assert!(matches!(err, CodegenError::SentinelKindMismatch { .. }));
    }

    #[test]
    fn check_plan_clean_tree_is_clean() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();
        let report = check_plan(tmp.path(), &plan, options()).unwrap();
        assert!(report.is_clean());
    }

    #[test]
    fn check_plan_reports_differing_file() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();
        let plan_drift = vec![plan_owned("src/main.rs", "fn main() { /* drift */ }\n")];
        let report = check_plan(tmp.path(), &plan_drift, options()).unwrap();
        assert_eq!(report.differing, vec![PathBuf::from("src/main.rs")]);
        assert!(report.missing.is_empty());
    }

    #[test]
    fn check_plan_reports_missing_file() {
        let tmp = TempDir::new().unwrap();
        // Put the directory into managed state first.
        ensure_sentinel(tmp.path(), SentinelKind::Hooks).unwrap();
        let plan = vec![plan_owned("src/main.rs", "fn main() {}\n")];
        let report = check_plan(tmp.path(), &plan, options()).unwrap();
        assert_eq!(report.missing, vec![PathBuf::from("src/main.rs")]);
    }

    #[test]
    fn check_plan_reports_orphans() {
        let tmp = TempDir::new().unwrap();
        let plan_v1 = vec![
            plan_owned("proto/a.proto", "a\n"),
            plan_owned("proto/b.proto", "b\n"),
        ];
        write_plan(tmp.path(), &plan_v1, options()).unwrap();
        let plan_v2 = vec![plan_owned("proto/a.proto", "a\n")];
        let report = check_plan(tmp.path(), &plan_v2, options()).unwrap();
        assert_eq!(report.orphaned, vec![PathBuf::from("proto/b.proto")]);
    }

    #[test]
    fn check_plan_ignores_preserve_files() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![plan_preserve("src/user.rs", "// a\n")];
        write_plan(tmp.path(), &plan, options()).unwrap();
        // Drift the preserve file — should NOT show up in the report.
        fs::write(tmp.path().join("src/user.rs"), "// b\n").unwrap();
        let report = check_plan(tmp.path(), &plan, options()).unwrap();
        assert!(report.is_clean());
    }
}
