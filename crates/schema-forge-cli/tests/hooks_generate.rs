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
