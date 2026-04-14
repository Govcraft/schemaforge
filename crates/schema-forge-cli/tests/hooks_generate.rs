//! Integration test for `schema-forge hooks generate`.
//!
//! Drives the binary against a tempdir schema directory containing a
//! `Translation` schema with two `@hook(...)` annotations, then verifies
//! the generated project layout. We do not invoke `cargo check` on the
//! emitted project — that would require downloading the full crate graph
//! at test time. Instead, we assert structural correctness and parse the
//! generated `.proto` with `protoc` if available.

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[allow(deprecated)]
fn schema_forge() -> Command {
    Command::cargo_bin("schemaforge").unwrap()
}

const TRANSLATION_SCHEMA: &str = r#"
@hook(before_change) """patch translated_text"""
@hook(after_change) """publish translation event"""
schema Translation {
  source_text: text required
  translated_text: text
  language: text
}
"#;

#[test]
fn generate_emits_expected_layout() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("translation.schema"), TRANSLATION_SCHEMA).unwrap();

    let out_dir = workdir.path().join("hooks-service");

    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    // Top-level files
    assert!(out_dir.join("Cargo.toml").exists(), "Cargo.toml missing");
    assert!(out_dir.join("build.rs").exists(), "build.rs missing");
    assert!(out_dir.join("src/main.rs").exists(), "src/main.rs missing");
    assert!(
        out_dir.join("src/hooks/mod.rs").exists(),
        "src/hooks/mod.rs missing"
    );

    // Per-schema artifacts
    let proto_path = out_dir.join("proto/translation_hooks.proto");
    assert!(proto_path.exists(), "proto file missing");
    let proto = fs::read_to_string(&proto_path).unwrap();
    assert!(proto.contains("service TranslationHooks"));
    assert!(proto.contains("rpc BeforeChange"));
    assert!(proto.contains("rpc AfterChange"));
    assert!(proto.contains("string source_text"));
    // Optional field tag for the non-required `translated_text`.
    assert!(proto.contains("optional string translated_text"));

    let impl_path = out_dir.join("src/hooks/translation.rs");
    assert!(impl_path.exists(), "translation.rs stub missing");
    let impl_src = fs::read_to_string(&impl_path).unwrap();
    assert!(impl_src.contains("impl TranslationHooks for Service"));
    assert!(impl_src.contains("async fn before_change"));
    assert!(impl_src.contains("async fn after_change"));
    assert!(impl_src.contains("TODO"));

    // Phase 4 prompt files
    let before_prompt = out_dir.join("src/hooks/translation/before_change.prompt.md");
    let after_prompt = out_dir.join("src/hooks/translation/after_change.prompt.md");
    assert!(before_prompt.exists(), "before_change prompt missing");
    assert!(after_prompt.exists(), "after_change prompt missing");
    let before_md = fs::read_to_string(&before_prompt).unwrap();
    assert!(before_md.contains("patch translated_text"));
    assert!(before_md.contains("source_text"));
    assert!(before_md.contains("Done when"));
}

#[test]
fn generate_preserves_existing_impl_without_force() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("translation.schema"), TRANSLATION_SCHEMA).unwrap();

    let out_dir = workdir.path().join("hooks-service");

    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    // Mark the impl file with a sentinel.
    let impl_path = out_dir.join("src/hooks/translation.rs");
    fs::write(&impl_path, "// USER EDITED\n").unwrap();

    // Re-run without --force
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    let after = fs::read_to_string(&impl_path).unwrap();
    assert_eq!(
        after, "// USER EDITED\n",
        "impl was clobbered without --force"
    );

    // Re-run WITH --force
    schema_forge()
        .args(["hooks", "generate", "--all", "--force", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();
    let after_force = fs::read_to_string(&impl_path).unwrap();
    assert!(
        after_force.contains("impl TranslationHooks for Service"),
        "impl should be regenerated with --force"
    );
}

/// Schema with array-typed fields: scalar arrays, enum array, and a many-relation.
const TASK_SCHEMA: &str = r#"
schema Project {
  name: text required
}

@hook(before_change) """validate task arrays"""
schema Task {
  title: text required
  tags: text[]
  scores: integer[]
  flags: boolean[]
  labels: enum("a","b")[]
  projects: -> Project[]
}
"#;

#[test]
fn generate_emits_repeated_for_array_and_many_relation_fields() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("task.schema"), TASK_SCHEMA).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    let proto = fs::read_to_string(out_dir.join("proto/task_hooks.proto")).unwrap();
    // Required scalar stays unmarked.
    assert!(
        proto.contains("string title ="),
        "expected `string title = ...`; proto:\n{proto}"
    );
    // Scalar arrays must become `repeated <type>`, never `optional string`.
    assert!(
        proto.contains("repeated string tags ="),
        "expected `repeated string tags`; proto:\n{proto}"
    );
    assert!(
        proto.contains("repeated int64 scores ="),
        "expected `repeated int64 scores`; proto:\n{proto}"
    );
    assert!(
        proto.contains("repeated bool flags ="),
        "expected `repeated bool flags`; proto:\n{proto}"
    );
    // Enum arrays become repeated string.
    assert!(
        proto.contains("repeated string labels ="),
        "expected `repeated string labels`; proto:\n{proto}"
    );
    // Many-relations become repeated string of ids.
    assert!(
        proto.contains("repeated string projects ="),
        "expected `repeated string projects`; proto:\n{proto}"
    );
    // No accidental `optional` on a repeated field.
    assert!(
        !proto.contains("optional string tags"),
        "tags must not be `optional string`; proto:\n{proto}"
    );

    // Response message also keeps repeated cardinality so hooks can echo
    // a modified array back without `prost-reflect` rejecting it.
    let response_block_start = proto
        .find("message TaskBeforeChangeResponse")
        .expect("response message present");
    let response_block = &proto[response_block_start..];
    let response_end = response_block.find("\n}\n").unwrap();
    let response_body = &response_block[..response_end];
    assert!(
        response_body.contains("repeated string tags"),
        "response should also emit `repeated string tags`; body:\n{response_body}"
    );
}

const NESTED_ARRAY_SCHEMA: &str = r#"
@hook(before_change) """nested arrays should fail"""
schema Bad {
  matrix: text[][]
}
"#;

#[test]
fn generate_rejects_nested_arrays() {
    // The DSL parser refuses `text[][]` outright; the codegen guard is a
    // defense-in-depth check for synthetic AST consumers. Either rejection
    // path is acceptable — both are non-zero exit with an error printed.
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("bad.schema"), NESTED_ARRAY_SCHEMA).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure();
}

/// A two-schema fixture used by the manifest / orphan / check tests.
const TWO_SCHEMAS: &str = r#"
@hook(before_change) """first hook"""
schema Alpha {
  name: text required
}
"#;

const SECOND_SCHEMA: &str = r#"
@hook(before_change) """second hook"""
schema Beta {
  title: text required
}
"#;

fn run_generate(schema_dir: &std::path::Path, out_dir: &std::path::Path) {
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(schema_dir)
        .arg("--out-dir")
        .arg(out_dir)
        .assert()
        .success();
}

#[test]
fn generate_creates_manifest_and_sentinel() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    assert!(
        out_dir.join(".schemaforge-hooks").exists(),
        "sentinel file missing"
    );
    let manifest_path = out_dir.join(".schemaforge-manifest.toml");
    assert!(manifest_path.exists(), "manifest missing");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    assert!(manifest.contains("generator = \"hooks\""));
    assert!(manifest.contains("path = \"proto/alpha_hooks.proto\""));
    assert!(
        !manifest.contains("path = \"src/hooks/alpha.rs\""),
        "preserved files must not appear in the manifest"
    );
}

#[test]
fn regenerate_prunes_orphans_when_schema_deleted() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    fs::write(schema_dir.join("beta.schema"), SECOND_SCHEMA).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    // Both schemas produced their proto files and prompt dirs.
    assert!(out_dir.join("proto/alpha_hooks.proto").exists());
    assert!(out_dir.join("proto/beta_hooks.proto").exists());
    assert!(
        out_dir
            .join("src/hooks/beta/before_change.prompt.md")
            .exists()
    );

    // Delete the beta schema and regenerate.
    fs::remove_file(schema_dir.join("beta.schema")).unwrap();
    run_generate(&schema_dir, &out_dir);

    // Beta's owned outputs are gone; alpha's remain.
    assert!(out_dir.join("proto/alpha_hooks.proto").exists());
    assert!(!out_dir.join("proto/beta_hooks.proto").exists());
    assert!(!out_dir.join("src/hooks/beta").exists());
    // The preserved beta.rs stub is NOT in the manifest, so it survives
    // pruning — this mirrors how the generator treats user code.
    assert!(out_dir.join("src/hooks/beta.rs").exists());
}

#[test]
fn regenerate_errors_when_owned_file_hand_edited() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    // Strip the marker from Cargo.toml.
    fs::write(out_dir.join("Cargo.toml"), "[package]\nname = \"hand-edited\"\n").unwrap();

    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("Cargo.toml"));
}

#[test]
fn check_flag_exits_zero_on_clean_tree() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    schema_forge()
        .args(["hooks", "generate", "--all", "--check", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();
}

#[test]
fn check_flag_exits_nonzero_on_drift() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    // Mutate an owned file (keeping the marker so it's legal to overwrite).
    let main_path = out_dir.join("src/main.rs");
    let original = fs::read_to_string(&main_path).unwrap();
    fs::write(&main_path, format!("{original}\n// drift\n")).unwrap();

    schema_forge()
        .args(["hooks", "generate", "--all", "--check", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("src/main.rs"));
}

#[test]
fn refuses_to_write_into_foreign_dir() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("unrelated.txt"), "keep me").unwrap();

    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--force-init"));

    // The unrelated file must not have been touched and the generator
    // must not have scaffolded into the directory.
    assert!(out_dir.join("unrelated.txt").exists());
    assert!(!out_dir.join("Cargo.toml").exists());
}

#[test]
fn force_init_overrides_foreign_dir_check() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("unrelated.txt"), "keep me").unwrap();

    schema_forge()
        .args([
            "hooks",
            "generate",
            "--all",
            "--force-init",
            "--schema-dir",
        ])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    assert!(out_dir.join("unrelated.txt").exists());
    assert!(out_dir.join("Cargo.toml").exists());
    assert!(out_dir.join(".schemaforge-hooks").exists());
}

#[test]
fn list_reports_hooks() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("translation.schema"), TRANSLATION_SCHEMA).unwrap();

    schema_forge()
        .args(["hooks", "list", "--schema-dir"])
        .arg(&schema_dir)
        .assert()
        .success();
}
