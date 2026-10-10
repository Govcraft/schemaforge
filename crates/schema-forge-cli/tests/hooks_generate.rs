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
    assert!(proto.contains("import \"google/protobuf/struct.proto\";"));
    assert!(proto.contains("repeated string changed_fields = 4;"));
    assert!(proto.contains("map<string, google.protobuf.Value> previous = 5;"));
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(out_dir.join("Cargo.toml")).unwrap()).unwrap();
    assert_eq!(
        manifest["dependencies"]["prost-types"].as_str(),
        Some("0.14.4")
    );

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
    let after_md = fs::read_to_string(&after_prompt).unwrap();
    assert!(after_md.contains("changed_fields"));
    assert!(after_md.contains("previous"));
    assert!(after_md.contains("Both collections are empty on create and no-op writes"));
}

#[test]
fn generated_change_metadata_avoids_business_field_and_json_name_collisions() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(
        schema_dir.join("collision.schema"),
        r#"
@hook(after_change) """act only on persisted changes"""
schema Collision {
  changed_fields: text
  previous: text
  schemaforge_changed_fields_2: text
  schemaforge_previous_2: text
}
"#,
    )
    .unwrap();
    let out_dir = workdir.path().join("hooks-service");
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();
    let proto_path = out_dir.join("proto/collision_hooks.proto");
    let proto = fs::read_to_string(&proto_path).unwrap();
    assert!(proto.contains("repeated string schemaforge_changed_fields_3 = 4;"));
    assert!(proto.contains("map<string, google.protobuf.Value> schemaforge_previous_3 = 5;"));
    assert!(proto.contains("optional string changed_fields = 100;"));
    assert!(proto.contains("optional string previous = 101;"));
    let prompt =
        fs::read_to_string(out_dir.join("src/hooks/collision/after_change.prompt.md")).unwrap();
    assert!(prompt.contains("`schemaforge_changed_fields_3` lists"));
    assert!(prompt.contains("`schemaforge_previous_3` contains"));
    let output = std::process::Command::new("protoc")
        .arg("--descriptor_set_out")
        .arg(workdir.path().join("collision.bin"))
        .arg("--proto_path")
        .arg(out_dir.join("proto"))
        .arg(&proto_path)
        .output()
        .expect("protoc is required to validate generated hook protobuf");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The scaffold must serve through `acton-service`, not a bare tonic server.
///
/// This is the regression that shipped: the emitted project had no
/// `acton-service` dependency at all, so `[token]` in its config authenticated
/// nothing and every RPC — carrying entity fields and the triggering user's
/// subject — answered whoever could reach the port. The failure is invisible
/// from the outside (the RPCs work either way), so it needs a test rather than
/// a reviewer noticing.
#[test]
fn generate_emits_an_authenticated_service() {
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

    let cargo_toml = fs::read_to_string(out_dir.join("Cargo.toml")).unwrap();
    let generated_manifest: toml::Value = toml::from_str(&cargo_toml).unwrap();
    let cli_manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    let runtime_manifest: toml::Value =
        toml::from_str(include_str!("../../schema-forge-acton/Cargo.toml")).unwrap();
    assert_eq!(
        cli_manifest["dependencies"]["acton-service"]["version"],
        runtime_manifest["dependencies"]["acton-service"]["version"],
        "CLI and runtime must use the same token implementation"
    );
    assert_eq!(
        generated_manifest["dependencies"]["acton-service"]["version"],
        cli_manifest["dependencies"]["acton-service"]["version"],
        "hook services must use the forge's acton-service version"
    );

    let main_rs = fs::read_to_string(out_dir.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("ServiceBuilder::new()") && main_rs.contains("with_grpc_services"),
        "scaffold must serve through ServiceBuilder — that is what applies the \
         token layer:\n{main_rs}"
    );
    assert!(
        !main_rs.contains("Server::builder()"),
        "a bare tonic Server bypasses every auth layer:\n{main_rs}"
    );
    // `GrpcServicesBuilder::build` is generic over the state type since
    // acton-service 0.46.0, so nothing else pins `Config`'s type parameter
    // and a bare `Config::load()` fails to compile (E0283).
    assert!(
        main_rs.contains("let config = Config::<()>::load()?;"),
        "scaffold must name its config type:\n{main_rs}"
    );
    // Reflection is auth-exempt, so enabling it would publish the hook message
    // definitions — and therefore the entity field names — unauthenticated.
    // Matched as a builder call, not as a substring — the scaffold's comments
    // name `.with_reflection()` as the way to opt in.
    assert!(
        !main_rs
            .lines()
            .any(|l| l.trim_start().starts_with(".with_reflection()")),
        "reflection must stay opt-in:\n{main_rs}"
    );

    // Load with the framework so its strict config validation also covers
    // generated tables. Token and gRPC sections must be active, not comments.
    let config_path = out_dir.join("config.toml");
    let contents = fs::read_to_string(&config_path).expect("generated configuration");
    let parsed: toml::Value = toml::from_str(&contents).expect("valid generated TOML");
    assert_eq!(parsed["grpc"]["enabled"].as_bool(), Some(true));
    assert_eq!(parsed["token"]["format"].as_str(), Some("paseto"));
    #[cfg(feature = "server")]
    {
        let config = acton_service::config::Config::<()>::load_from(config_path.to_str().unwrap())
            .expect("scaffold must emit valid framework configuration");
        assert!(
            matches!(
                config.token,
                Some(acton_service::config::TokenConfig::Paseto(_))
            ),
            "scaffold must configure PASETO token auth"
        );
        assert!(
            config.grpc.is_some_and(|grpc| grpc.enabled),
            "scaffold must enable gRPC or ServiceBuilder refuses the build"
        );
    }
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
    // An older scaffold has no prost-types dependency. Normal regeneration
    // preserves its manifest, so the documented cargo add step is required.
    let cargo_path = out_dir.join("Cargo.toml");
    let older_manifest = fs::read_to_string(&cargo_path)
        .unwrap()
        .replace("prost-types = \"0.14.4\"\n", "");
    fs::write(&cargo_path, &older_manifest).unwrap();

    // Re-run without --force
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    let after = fs::read_to_string(&impl_path).unwrap();
    assert_eq!(fs::read_to_string(&cargo_path).unwrap(), older_manifest);
    let proto = fs::read_to_string(out_dir.join("proto/translation_hooks.proto")).unwrap();
    assert!(proto.contains("map<string, google.protobuf.Value> previous = 5;"));
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
    assert!(out_dir
        .join("src/hooks/beta/before_change.prompt.md")
        .exists());

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

    // Strip the marker from an Owned file. Issue #41 moved main.rs and
    // mod.rs to Preserve (they now splice additively), so pick a still-
    // Owned proto file to exercise the marker enforcement path.
    fs::write(
        out_dir.join("proto/alpha_hooks.proto"),
        "// hand-edited, marker stripped\n",
    )
    .unwrap();

    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("alpha_hooks.proto"));
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

    // Mutate an Owned file (keeping the marker so it's legal to
    // overwrite). Issue #41 moved main.rs and mod.rs to Preserve, so
    // exercise the drift detector against a still-Owned proto file.
    let proto_path = out_dir.join("proto/alpha_hooks.proto");
    let original = fs::read_to_string(&proto_path).unwrap();
    fs::write(&proto_path, format!("{original}\n// drift\n")).unwrap();

    schema_forge()
        .args(["hooks", "generate", "--all", "--check", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("alpha_hooks.proto"));
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
        .args(["hooks", "generate", "--all", "--force-init", "--schema-dir"])
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
fn issue_41_additive_mode_inserts_new_schema_without_touching_customized_main() {
    // Acceptance criterion from #41: adding a net-new @hook schema must
    // insert the new module + service wiring into the existing main.rs
    // and mod.rs without clobbering user customizations outside the
    // insertion markers.
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    // User customizes main.rs: adds a top-of-file mod declaration and a
    // custom module import. The kind of edit the real engage project
    // accumulated over time — `mod api; mod cpm; mod guard;` and friends.
    let main_path = out_dir.join("src/main.rs");
    let original = fs::read_to_string(&main_path).unwrap();
    let customized = original.replace("mod hooks;\n", "mod hooks;\nmod api;\nmod guard;\n");
    assert_ne!(customized, original, "customization must actually differ");
    fs::write(&main_path, &customized).unwrap();

    // Same for mod.rs — add a user-owned module declaration outside the
    // insertion markers.
    let mod_path = out_dir.join("src/hooks/mod.rs");
    let mod_original = fs::read_to_string(&mod_path).unwrap();
    fs::write(&mod_path, format!("{mod_original}\npub mod shared;\n")).unwrap();

    // Now add a net-new schema and re-run without any flags. Additive
    // mode should pick it up.
    fs::write(schema_dir.join("beta.schema"), SECOND_SCHEMA).unwrap();
    run_generate(&schema_dir, &out_dir);

    // User customizations survived.
    let main_after = fs::read_to_string(&main_path).unwrap();
    assert!(
        main_after.contains("mod api;"),
        "user's `mod api;` customization must survive:\n{main_after}"
    );
    assert!(
        main_after.contains("mod guard;"),
        "user's `mod guard;` customization must survive"
    );
    let mod_after = fs::read_to_string(&mod_path).unwrap();
    assert!(
        mod_after.contains("pub mod shared;"),
        "user's out-of-markers module must survive:\n{mod_after}"
    );

    // New schema is now wired through main.rs and mod.rs.
    assert!(
        main_after.contains("pb::beta::beta_hooks_server::BetaHooksServer"),
        "main.rs must add_service the new Beta hook:\n{main_after}"
    );
    assert!(
        main_after.contains("tonic::include_proto!(\"schema_forge_hooks.beta\")"),
        "main.rs must include the new Beta proto:\n{main_after}"
    );
    assert!(
        mod_after.contains("pub mod beta;"),
        "mod.rs must declare the new beta module:\n{mod_after}"
    );

    // And the existing Alpha wiring is still present.
    assert!(
        main_after.contains("pb::alpha::alpha_hooks_server::AlphaHooksServer"),
        "alpha wiring must remain"
    );
    assert!(mod_after.contains("pub mod alpha;"));

    // Per-schema Owned artifacts for the new schema were written.
    assert!(out_dir.join("proto/beta_hooks.proto").exists());
    assert!(out_dir
        .join("src/hooks/beta/before_change.prompt.md")
        .exists());
    // And the Preserve stub for Beta was scaffolded as a first-time file.
    assert!(out_dir.join("src/hooks/beta.rs").exists());
}

const CUSTOM_ALPHA_REGISTRATION: &str =
    "        // Keep dependency injection and constructor formatting.\n\
        .add_service(\n\
            pb :: alpha :: alpha_hooks_server :: AlphaHooksServer :: new(\n\
                hooks::alpha::Service::new(\n\
                    Box::new(provision::LoggingExecutor),\n\
                ),\n\
            ),\n\
        )\n";

const DEFAULT_ALPHA_REGISTRATION: &str = "        .add_service(pb::alpha::alpha_hooks_server::AlphaHooksServer::new(hooks::alpha::Service::default()))\n";

#[test]
fn additive_generation_preserves_dependency_wiring_inside_service_markers() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    let main_path = out_dir.join("src/main.rs");
    let original = fs::read_to_string(&main_path).unwrap();
    let customized = original.replace(DEFAULT_ALPHA_REGISTRATION, CUSTOM_ALPHA_REGISTRATION);
    assert_ne!(original, customized);
    fs::write(&main_path, &customized).unwrap();
    fs::write(schema_dir.join("beta.schema"), SECOND_SCHEMA).unwrap();
    run_generate(&schema_dir, &out_dir);

    let generated = fs::read_to_string(&main_path).unwrap();
    assert!(generated.contains(CUSTOM_ALPHA_REGISTRATION));
    assert_eq!(generated.matches("AlphaHooksServer").count(), 1);
    assert_eq!(generated.matches("BetaHooksServer").count(), 1);
    run_generate(&schema_dir, &out_dir);
    assert_eq!(fs::read_to_string(&main_path).unwrap(), generated);
    schema_forge()
        .args(["hooks", "generate", "--all", "--check", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();
}

#[test]
fn check_detects_preserve_marker_drift_without_writing() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);
    let main_path = out_dir.join("src/main.rs");
    let mod_path = out_dir.join("src/hooks/mod.rs");
    let old_main = fs::read_to_string(&main_path).unwrap();
    let old_mod = fs::read_to_string(&mod_path).unwrap();
    fs::write(schema_dir.join("beta.schema"), SECOND_SCHEMA).unwrap();
    run_generate(&schema_dir, &out_dir);
    // Keep all Owned artifacts current, but restore only user-owned wiring.
    fs::write(&main_path, &old_main).unwrap();
    fs::write(&mod_path, &old_mod).unwrap();
    schema_forge()
        .args(["hooks", "generate", "--all", "--check", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("src/main.rs"))
        .stderr(predicates::str::contains("src/hooks/mod.rs"));
    assert_eq!(fs::read_to_string(&main_path).unwrap(), old_main);
    assert_eq!(fs::read_to_string(&mod_path).unwrap(), old_mod);
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
fn additive_generation_ignores_commented_and_quoted_registrations() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);
    let main_path = out_dir.join("src/main.rs");
    let original = fs::read_to_string(&main_path).unwrap();
    let notes = concat!(
        "        // .add_service(pb::alpha::alpha_hooks_server::AlphaHooksServer::new(unused))\n",
        "        /* outer /* nested */ .add_service(pb::alpha::alpha_hooks_server::AlphaHooksServer::new(unused)) */\n",
        "        .with_note(\".add_service(pb::alpha::alpha_hooks_server::AlphaHooksServer::new(unused))\")\n",
        "        .with_note(r##\".add_service(pb::alpha::alpha_hooks_server::AlphaHooksServer::new(unused))\"##)\n",
    );
    fs::write(
        &main_path,
        original.replace(DEFAULT_ALPHA_REGISTRATION, notes),
    )
    .unwrap();
    run_generate(&schema_dir, &out_dir);
    let after = fs::read_to_string(&main_path).unwrap();
    assert!(
        after.contains(notes),
        "user comments and expressions must survive"
    );
    assert!(
        after.contains(DEFAULT_ALPHA_REGISTRATION),
        "an actual registration must be appended"
    );
    run_generate(&schema_dir, &out_dir);
    assert_eq!(fs::read_to_string(&main_path).unwrap(), after);
}

#[test]
fn additive_generation_removes_obsolete_calls_and_preserves_active_constructors() {
    for select_subset in [false, true] {
        let workdir = TempDir::new().unwrap();
        let schema_dir = workdir.path().join("schemas");
        fs::create_dir_all(&schema_dir).unwrap();
        fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
        fs::write(schema_dir.join("beta.schema"), SECOND_SCHEMA).unwrap();
        let out_dir = workdir.path().join("hooks-service");
        run_generate(&schema_dir, &out_dir);
        let main_path = out_dir.join("src/main.rs");
        let original = fs::read_to_string(&main_path).unwrap();
        let default_beta = DEFAULT_ALPHA_REGISTRATION
            .replace("alpha", "beta")
            .replace("Alpha", "Beta");
        let custom_beta = CUSTOM_ALPHA_REGISTRATION
            .replace("alpha", "beta")
            .replace("Alpha", "Beta");
        let customized = original
            .replace(DEFAULT_ALPHA_REGISTRATION, CUSTOM_ALPHA_REGISTRATION)
            .replace(&default_beta, &custom_beta);
        assert_ne!(original, customized);
        fs::write(&main_path, &customized).unwrap();
        let selection = if select_subset {
            ["--schema", "Beta"].as_slice()
        } else {
            fs::remove_file(schema_dir.join("alpha.schema")).unwrap();
            ["--all"].as_slice()
        };
        schema_forge()
            .args(["hooks", "generate"])
            .args(selection)
            .args(["--check", "--schema-dir"])
            .arg(&schema_dir)
            .arg("--out-dir")
            .arg(&out_dir)
            .assert()
            .failure();
        assert_eq!(fs::read_to_string(&main_path).unwrap(), customized);
        schema_forge()
            .args(["hooks", "generate"])
            .args(selection)
            .arg("--schema-dir")
            .arg(&schema_dir)
            .arg("--out-dir")
            .arg(&out_dir)
            .assert()
            .success();
        let after = fs::read_to_string(&main_path).unwrap();
        assert!(!after.contains("AlphaHooksServer"));
        assert!(!after.contains("hooks::alpha::Service::new"));
        assert!(!after.contains("schema_forge_hooks.alpha"));
        assert!(after.contains(&custom_beta));
        assert!(!fs::read_to_string(out_dir.join("src/hooks/mod.rs"))
            .unwrap()
            .contains("pub mod alpha;"));
        assert!(!out_dir.join("proto/alpha_hooks.proto").exists());
        schema_forge()
            .args(["hooks", "generate"])
            .args(selection)
            .arg("--schema-dir")
            .arg(&schema_dir)
            .arg("--out-dir")
            .arg(&out_dir)
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&main_path).unwrap(), after);
    }
}

#[test]
fn malformed_markers_fail_before_owned_artifacts_are_written() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);
    let main_path = out_dir.join("src/main.rs");
    let malformed = fs::read_to_string(&main_path)
        .unwrap()
        .replace("// SCHEMAFORGE_HOOKS_SERVICES_END", "// removed end marker");
    fs::write(&main_path, &malformed).unwrap();
    fs::write(schema_dir.join("beta.schema"), SECOND_SCHEMA).unwrap();
    for check in [true, false] {
        let mut command = schema_forge();
        command.args(["hooks", "generate", "--all"]);
        if check {
            command.arg("--check");
        }
        command
            .arg("--schema-dir")
            .arg(&schema_dir)
            .arg("--out-dir")
            .arg(&out_dir)
            .assert()
            .failure()
            .stderr(predicates::str::contains("no matching"));
        assert_eq!(fs::read_to_string(&main_path).unwrap(), malformed);
        assert!(!out_dir.join("proto/beta_hooks.proto").exists());
        assert!(!out_dir.join("src/hooks/beta.rs").exists());
    }
}

#[test]
fn forced_check_reports_preserve_rewrites_and_leaves_files_unchanged() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);
    let main_path = out_dir.join("src/main.rs");
    let customized = fs::read_to_string(&main_path)
        .unwrap()
        .replace(DEFAULT_ALPHA_REGISTRATION, CUSTOM_ALPHA_REGISTRATION);
    fs::write(&main_path, &customized).unwrap();
    let cargo_path = out_dir.join("Cargo.toml");
    let custom_cargo = format!(
        "{}\n# custom dependency configuration\n",
        fs::read_to_string(&cargo_path).unwrap()
    );
    fs::write(&cargo_path, &custom_cargo).unwrap();
    for flag in ["--regenerate", "--force-user-files"] {
        schema_forge()
            .args([
                "hooks",
                "generate",
                "--all",
                "--check",
                flag,
                "--schema-dir",
            ])
            .arg(&schema_dir)
            .arg("--out-dir")
            .arg(&out_dir)
            .assert()
            .failure()
            .stderr(predicates::str::contains("src/main.rs"))
            .stderr(predicates::str::contains("Cargo.toml"));
        assert_eq!(fs::read_to_string(&main_path).unwrap(), customized);
        assert_eq!(fs::read_to_string(&cargo_path).unwrap(), custom_cargo);
    }
}

#[test]
fn check_reports_legacy_upgrades_and_missing_preserve_files() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();
    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);
    let main_path = out_dir.join("src/main.rs");
    let mod_path = out_dir.join("src/hooks/mod.rs");
    let legacy = "// @generated by schema-forge hooks\npub mod alpha;\n";
    fs::write(&main_path, legacy).unwrap();
    fs::write(&mod_path, legacy).unwrap();
    fs::remove_file(out_dir.join("src/hooks/alpha.rs")).unwrap();
    schema_forge()
        .args(["hooks", "generate", "--all", "--check", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("src/main.rs"))
        .stderr(predicates::str::contains("src/hooks/mod.rs"))
        .stderr(predicates::str::contains("src/hooks/alpha.rs"));
    assert_eq!(fs::read_to_string(&main_path).unwrap(), legacy);
    assert_eq!(fs::read_to_string(&mod_path).unwrap(), legacy);
    assert!(!out_dir.join("src/hooks/alpha.rs").exists());
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
fn issue_41_additive_mode_upgrades_legacy_owned_mod_rs() {
    // Before #41, `src/hooks/mod.rs` was Owned with an `@generated` header
    // and no insertion markers. Existing projects that upgrade the CLI
    // must have their mod.rs transparently migrated to the new
    // marker-bounded layout on the next run — no --regenerate required.
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    // Simulate a legacy mod.rs: replace the current content with a
    // marker-less body that carries the old `@generated` header.
    let mod_path = out_dir.join("src/hooks/mod.rs");
    fs::write(
        &mod_path,
        "// @generated by schema-forge hooks — DO NOT EDIT\n\
         //! Per-schema hook service implementations.\n\n\
         pub mod alpha;\n",
    )
    .unwrap();

    // Re-run additively.
    run_generate(&schema_dir, &out_dir);

    let mod_after = fs::read_to_string(&mod_path).unwrap();
    assert!(
        mod_after.contains("SCHEMAFORGE_HOOKS_MODS_BEGIN"),
        "legacy mod.rs must be upgraded to marker-bounded layout:\n{mod_after}"
    );
    assert!(mod_after.contains("pub mod alpha;"));
}

#[test]
fn issue_41_regenerate_flag_rewrites_preserve_files() {
    // `--regenerate` is the escape hatch for full rewrites, subsuming
    // `--force-user-files`. Customized main.rs and mod.rs get clobbered
    // back to the current scaffold.
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(schema_dir.join("alpha.schema"), TWO_SCHEMAS).unwrap();

    let out_dir = workdir.path().join("hooks-service");
    run_generate(&schema_dir, &out_dir);

    let main_path = out_dir.join("src/main.rs");
    fs::write(&main_path, "// custom\n").unwrap();

    schema_forge()
        .args(["hooks", "generate", "--all", "--regenerate", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();

    let after = fs::read_to_string(&main_path).unwrap();
    assert!(
        after.contains("SCHEMAFORGE_HOOKS_PB_BEGIN"),
        "--regenerate must rewrite main.rs from the current scaffold:\n{after}"
    );
    assert!(!after.contains("// custom\n"));
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

#[test]
fn generate_multiline_intents_are_rust_doc_comments() {
    let workdir = TempDir::new().unwrap();
    let schema_dir = workdir.path().join("schemas");
    fs::create_dir_all(&schema_dir).unwrap();
    fs::write(
        schema_dir.join("deployment.schema"),
        r#"@hook(before_change) """
Validate the deployment.
Reject an invalid transition.
"""
@hook(after_change) """
Provision or reconcile the tenant's process for this deployment.
On update: reconcile state transitions.

Keep `name` stable.
"""
schema Deployment { name: text required }
"#,
    )
    .unwrap();
    let out_dir = workdir.path().join("hooks-service");
    schema_forge()
        .args(["hooks", "generate", "--all", "--schema-dir"])
        .arg(&schema_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .assert()
        .success();
    let source = fs::read_to_string(out_dir.join("src/hooks/deployment.rs")).unwrap();
    assert!(
        source.contains(
            "    /// Validate the deployment.\n    /// Reject an invalid transition.\n    async fn before_change"
        ),
        "before-change intent must remain inside its doc comment:\n{source}"
    );
    assert!(
        source.contains(
            "    /// Provision or reconcile the tenant's process for this deployment.\n    /// On update: reconcile state transitions.\n    /// \n    /// Keep `name` stable.\n    async fn after_change"
        ),
        "after-change intent must remain inside its doc comment:\n{source}"
    );
    let prompt =
        fs::read_to_string(out_dir.join("src/hooks/deployment/after_change.prompt.md")).unwrap();
    assert!(prompt.contains("On update: reconcile state transitions."));
    assert!(prompt.contains("Keep `name` stable."));
}
