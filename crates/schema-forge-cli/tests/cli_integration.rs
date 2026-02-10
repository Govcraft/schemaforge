use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Helper to get the schema-forge binary command.
#[allow(deprecated)]
fn schema_forge() -> Command {
    Command::cargo_bin("schema-forge").unwrap()
}

// ---------------------------------------------------------------------------
// Help and version tests
// ---------------------------------------------------------------------------

#[test]
fn help_exits_zero() {
    schema_forge()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Adaptive Object Model"));
}

#[test]
fn version_exits_zero() {
    schema_forge()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("schema-forge"));
}

#[test]
fn init_help() {
    schema_forge()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Initialize a new SchemaForge project",
        ));
}

#[test]
fn parse_help() {
    schema_forge()
        .args(["parse", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parse and validate"));
}

#[test]
fn apply_help() {
    schema_forge()
        .args(["apply", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Apply .schema files"));
}

#[test]
fn migrate_help() {
    schema_forge()
        .args(["migrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Plan and execute"));
}

#[test]
fn inspect_help() {
    schema_forge()
        .args(["inspect", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Inspect registered schemas"));
}

#[test]
fn generate_help() {
    schema_forge()
        .args(["generate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Generate schemas"));
}

#[test]
fn serve_help() {
    schema_forge()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start acton-service"));
}

#[test]
fn export_openapi_help() {
    schema_forge()
        .args(["export", "openapi", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Export OpenAPI"));
}

#[test]
fn policies_list_help() {
    schema_forge()
        .args(["policies", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List generated Cedar"));
}

#[test]
fn completions_help() {
    schema_forge()
        .args(["completions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("shell completion"));
}

// ---------------------------------------------------------------------------
// Completions tests
// ---------------------------------------------------------------------------

#[test]
fn completions_bash() {
    schema_forge()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn completions_zsh() {
    schema_forge()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn completions_fish() {
    schema_forge()
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn completions_powershell() {
    schema_forge()
        .args(["completions", "powershell"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn completions_elvish() {
    schema_forge()
        .args(["completions", "elvish"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn completions_invalid_shell_rejected() {
    schema_forge()
        .args(["completions", "tcsh"])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// Init command tests
// ---------------------------------------------------------------------------

#[test]
fn init_creates_project_directory() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("my-project");

    schema_forge()
        .args([
            "init",
            project_name.to_str().unwrap(),
            "-t",
            "minimal",
            "-y",
        ])
        .assert()
        .success();

    assert!(project_name.join("schemas").exists());
    assert!(project_name.join("schemas/example.schema").exists());
    assert!(project_name.join("config.toml").exists());
}

#[test]
fn init_full_template() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("full-project");

    schema_forge()
        .args(["init", project_name.to_str().unwrap(), "-t", "full", "-y"])
        .assert()
        .success();

    assert!(project_name.join("schemas").exists());
    assert!(project_name.join("policies/generated").exists());
    assert!(project_name.join("policies/custom").exists());
    assert!(project_name.join("Dockerfile").exists());
    assert!(project_name.join("k8s").exists());
}

#[test]
fn init_api_only_template() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("api-project");

    schema_forge()
        .args([
            "init",
            project_name.to_str().unwrap(),
            "-t",
            "api-only",
            "-y",
        ])
        .assert()
        .success();

    assert!(project_name.join("schemas").exists());
    assert!(project_name.join("policies/generated").exists());
    assert!(!project_name.join("Dockerfile").exists());
}

#[test]
fn init_fails_if_directory_exists_without_force() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("existing-project");
    fs::create_dir_all(&project_name).unwrap();

    schema_forge()
        .args([
            "init",
            project_name.to_str().unwrap(),
            "-t",
            "minimal",
            "-y",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn init_succeeds_with_force_over_existing() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("force-project");
    fs::create_dir_all(&project_name).unwrap();

    schema_forge()
        .args([
            "init",
            project_name.to_str().unwrap(),
            "-t",
            "minimal",
            "-y",
            "--force",
        ])
        .assert()
        .success();
}

#[test]
fn init_invalid_template_rejected() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("bad-template");

    schema_forge()
        .args([
            "init",
            project_name.to_str().unwrap(),
            "-t",
            "nonexistent",
            "-y",
        ])
        .assert()
        .failure();
}

#[test]
fn init_json_output() {
    let dir = TempDir::new().unwrap();
    let project_name = dir.path().join("json-project");

    schema_forge()
        .args([
            "--format",
            "json",
            "init",
            project_name.to_str().unwrap(),
            "-t",
            "minimal",
            "-y",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"template\""));
}

// ---------------------------------------------------------------------------
// Parse command tests
// ---------------------------------------------------------------------------

#[test]
fn parse_valid_schema_file() {
    let dir = TempDir::new().unwrap();
    let schema_path = dir.path().join("test.schema");
    fs::write(
        &schema_path,
        "schema Contact {\n    name: text(max: 255) required\n    email: text required indexed\n}\n",
    )
    .unwrap();

    schema_forge()
        .args(["parse", schema_path.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn parse_invalid_schema_file() {
    let dir = TempDir::new().unwrap();
    let schema_path = dir.path().join("bad.schema");
    fs::write(&schema_path, "schema contact {\n    name: text\n}\n").unwrap();

    schema_forge()
        .args(["parse", schema_path.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn parse_missing_file() {
    schema_forge()
        .args(["parse", "/nonexistent/path.schema"])
        .assert()
        .failure();
}

#[test]
fn parse_with_print_flag() {
    let dir = TempDir::new().unwrap();
    let schema_path = dir.path().join("round-trip.schema");
    fs::write(
        &schema_path,
        "schema Contact {\n    name: text(max: 255) required\n}\n",
    )
    .unwrap();

    schema_forge()
        .args(["parse", "--print", schema_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("schema Contact"));
}

#[test]
fn parse_json_format() {
    let dir = TempDir::new().unwrap();
    let schema_path = dir.path().join("json-test.schema");
    fs::write(
        &schema_path,
        "schema Contact {\n    name: text required\n}\n",
    )
    .unwrap();

    schema_forge()
        .args(["--format", "json", "parse", schema_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schemas\""));
}

#[test]
fn parse_directory_with_schema_files() {
    let dir = TempDir::new().unwrap();
    let schemas_dir = dir.path().join("schemas");
    fs::create_dir_all(&schemas_dir).unwrap();
    fs::write(
        schemas_dir.join("a.schema"),
        "schema Alpha {\n    name: text required\n}\n",
    )
    .unwrap();
    fs::write(
        schemas_dir.join("b.schema"),
        "schema Beta {\n    value: integer required\n}\n",
    )
    .unwrap();

    schema_forge()
        .args(["parse", schemas_dir.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn parse_empty_directory_fails() {
    let dir = TempDir::new().unwrap();
    let empty_dir = dir.path().join("empty");
    fs::create_dir_all(&empty_dir).unwrap();

    schema_forge()
        .args(["parse", empty_dir.to_str().unwrap()])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// Global flag tests
// ---------------------------------------------------------------------------

#[test]
fn verbose_flag_accepted() {
    schema_forge().args(["-v", "--help"]).assert().success();
}

#[test]
fn quiet_flag_accepted() {
    schema_forge().args(["-q", "--help"]).assert().success();
}

#[test]
fn no_color_flag_accepted() {
    schema_forge()
        .args(["--no-color", "--help"])
        .assert()
        .success();
}

#[test]
fn format_json_flag_accepted() {
    schema_forge()
        .args(["--format", "json", "--help"])
        .assert()
        .success();
}

#[test]
fn format_plain_flag_accepted() {
    schema_forge()
        .args(["--format", "plain", "--help"])
        .assert()
        .success();
}

#[test]
fn invalid_format_rejected() {
    schema_forge()
        .args(["--format", "xml", "completions", "bash"])
        .assert()
        .failure();
}

#[test]
fn verbose_and_quiet_conflict() {
    schema_forge()
        .args(["-v", "-q", "completions", "bash"])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// No subcommand shows help
// ---------------------------------------------------------------------------

#[test]
fn no_subcommand_shows_error() {
    schema_forge()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}
