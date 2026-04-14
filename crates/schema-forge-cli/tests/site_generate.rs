//! Integration tests for `schema-forge site generate`.
//!
//! These tests verify the shape of the generated tree, idempotency of
//! regeneration, drift detection, Preserve-mode semantics, and the error
//! path for v0-unsupported field types. The shell-side smoke test
//! (see the task spec) covers `pnpm build`.

use std::fs;
use std::path::Path;

use assert_cmd::cargo_bin_cmd;
use assert_cmd::Command;
use tempfile::TempDir;

/// Minimal v0-friendly schema with every supported type exercised.
const V0_EMPLOYEE: &str = r#"
@display("full_name")
schema Employee {
    full_name:  text(max: 255) required
    email:      text(max: 512)
    age:        integer(min: 0)
    salary:     float(precision: 2)
    active:     boolean
    hire_date:  datetime required
    status:     enum("active", "on_leave", "terminated") default("active")
    department: -> Department
}

schema Department {
    name:       text(max: 255) required
}
"#;

/// Schema with an only-unsupported field — used to assert the clean error path.
const UNSUPPORTED_SCHEMA: &str = r#"
schema Bad {
    address: composite {
        street: text
        city:   text required
    }
}
"#;

fn write_schemas(dir: &Path, contents: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("test.schema"), contents).unwrap();
}

fn run_generate(schema_dir: &Path, out_dir: &Path, entity: &str, extra: &[&str]) -> Command {
    let mut cmd = cargo_bin_cmd!("schemaforge");
    cmd.arg("site")
        .arg("generate")
        .arg("-s")
        .arg(schema_dir)
        .arg("-o")
        .arg(out_dir)
        .arg("--schema")
        .arg(entity);
    for e in extra {
        cmd.arg(e);
    }
    cmd
}

#[test]
fn fresh_generate_emits_expected_tree() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();

    // Sentinel + manifest
    assert!(out_dir.join(".schemaforge-site").exists());
    assert!(out_dir.join(".schemaforge-manifest.toml").exists());

    // Root files
    for f in [
        "package.json",
        "vite.config.ts",
        "tsconfig.json",
        "tsconfig.node.json",
        "tailwind.config.ts",
        "index.html",
        ".gitignore",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }

    // src/ owned
    for f in [
        "src/main.tsx",
        "src/App.tsx",
        "src/index.css",
        "src/lib/utils.ts",
        "src/lib/auth.ts",
        "src/lib/require-auth.tsx",
        "src/components/ui/button.tsx",
        "src/components/ui/input.tsx",
        "src/components/ui/label.tsx",
        "src/components/ui/card.tsx",
        "src/components/ui/form.tsx",
        "src/components/ui/table.tsx",
        "src/generated/api-client.ts",
        "src/generated/entity-types.ts",
        "src/generated/zod-schemas.ts",
        "src/generated/route-manifest.ts",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }

    // Preserve pages
    for f in [
        "src/pages/login.tsx",
        "src/pages/employee/list.tsx",
        "src/pages/employee/detail.tsx",
        "src/pages/employee/edit.tsx",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }

    // Spot-check template substitutions
    let api = fs::read_to_string(out_dir.join("src/generated/api-client.ts")).unwrap();
    assert!(api.contains("listEmployees"));
    assert!(api.contains("const SCHEMA = \"Employee\""));
    // Task 4: client hits the versioned forge API prefix.
    assert!(api.contains("FORGE_API_PREFIX = \"/api/v1/forge\""));
    assert!(api.contains("${FORGE_API_PREFIX}/schemas/${SCHEMA}/entities"));
    // Task 4: updates go through PATCH, not PUT.
    assert!(api.contains("method: \"PATCH\""));
    assert!(!api.contains("method: \"PUT\""));
    // Task 4: the client threads the Bearer token through tokenStore.
    assert!(api.contains("tokenStore.get()"));
    assert!(api.contains("Bearer ${token}"));

    // Task 4: auth.ts exposes the expected surface.
    let auth_ts = fs::read_to_string(out_dir.join("src/lib/auth.ts")).unwrap();
    assert!(auth_ts.contains("/api/v1/forge/auth/login"));
    assert!(auth_ts.contains("export const tokenStore"));
    assert!(auth_ts.contains("export function isAuthenticated"));

    // Task 4: App.tsx wires RequireAuth and /login.
    let app_tsx = fs::read_to_string(out_dir.join("src/App.tsx")).unwrap();
    assert!(app_tsx.contains("<RequireAuth>"));
    assert!(app_tsx.contains("path=\"/login\""));
}

#[test]
fn regenerate_is_idempotent_via_check() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();
    run_generate(&schema_dir, &out_dir, "Employee", &["--check"])
        .assert()
        .success();
}

#[test]
fn tampering_with_owned_file_trips_check() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();

    let api_path = out_dir.join("src/generated/api-client.ts");
    let mut body = fs::read_to_string(&api_path).unwrap();
    body.push_str("// drift\n");
    fs::write(&api_path, body).unwrap();

    run_generate(&schema_dir, &out_dir, "Employee", &["--check"])
        .assert()
        .failure();
}

#[test]
fn missing_owned_file_trips_check() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();
    fs::remove_file(out_dir.join("src/generated/api-client.ts")).unwrap();

    run_generate(&schema_dir, &out_dir, "Employee", &["--check"])
        .assert()
        .failure();
}

#[test]
fn preserve_pages_survive_rerun() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();

    let list_path = out_dir.join("src/pages/employee/list.tsx");
    fs::write(&list_path, "// user edit\n").unwrap();

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();

    let after = fs::read_to_string(&list_path).unwrap();
    assert_eq!(after, "// user edit\n");
}

#[test]
fn login_page_is_preserve_mode() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();

    let login_path = out_dir.join("src/pages/login.tsx");
    fs::write(&login_path, "// hand-styled login page\n").unwrap();

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&login_path).unwrap(),
        "// hand-styled login page\n"
    );
}

#[test]
fn regenerate_after_fresh_build_passes_check() {
    // Idempotency across the new files: fresh generate → --check clean.
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();
    run_generate(&schema_dir, &out_dir, "Employee", &["--check"])
        .assert()
        .success();
}

#[test]
fn schema_with_only_unsupported_fields_errors_clearly() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, UNSUPPORTED_SCHEMA);

    let output = run_generate(&schema_dir, &out_dir, "Bad", &[])
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(
        err.contains("v0-supported fields") || err.contains("not yet supported"),
        "stderr: {err}"
    );
}

#[test]
fn missing_schema_name_errors_clearly() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    let output = run_generate(&schema_dir, &out_dir, "DoesNotExist", &[])
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(err.contains("not found"), "stderr: {err}");
}
