# SchemaForge v0.44.0 release

Release from merged main containing issues #151, #152, and #155. Use a new isolated worktree and signed Conventional Commits. Publish the already-tested acton-service 0.43.1 correlation fix before updating all four direct dependencies with cargo add, preserving features. Keep all other dependencies unchanged.

The product CLI moves from 0.43.0 to 0.44.0 and the independently versioned integration crate from 0.42.0 to 0.43.0. Owner read visibility and the generated Cedar request context are compatibility changes. Preserve explicit owner-only Read migration guidance with the placeholder guard and document the manual Cedar caller migration. Release notes describe only changes since v0.43.0; older unversioned changelog material is historical.

Validate the combined published dependency with workspace nextest, PostgreSQL integration checks, clippy with warnings denied, and real CLI PostgreSQL tests for authorization and default request correlation. Verify CLI and runtime release metadata. No broad formatting rewrite: inspect changed files and preserve the existing baseline.

Merge the release PR after CI, create a signed v0.44.0 tag on the merged commit, and use the existing six-target release workflow. Verify all published artifacts, checksums, signature identity, and the PostgreSQL binary version and metadata before declaring the release complete.
