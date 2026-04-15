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
// A schema tagged @system is always excluded from the site generator.
// Used as a "nothing to generate" fixture.
const UNSUPPORTED_SCHEMA: &str = r#"
@system
schema Bad {
    name: text required
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

    // Preserve pages — per-entity pages now live under src/app/pages/.
    for f in [
        "src/pages/login.tsx",
        "src/app/pages/employee/list.tsx",
        "src/app/pages/employee/detail.tsx",
        "src/app/pages/employee/edit.tsx",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }

    // Phase 2: admin shell scaffolds are emitted as Owned placeholders.
    for f in [
        "src/admin/layout.tsx",
        "src/admin/schemas-index.tsx",
        "src/admin/entity-list.tsx",
        "src/admin/entity-detail.tsx",
        "src/admin/entity-edit.tsx",
        "src/admin/api-client.ts",
        "src/admin/field-renderer.tsx",
        "src/admin/users-list.tsx",
        "src/admin/users-edit.tsx",
    ] {
        assert!(out_dir.join(f).exists(), "missing admin scaffold {f}");
    }

    // Spot-check template substitutions
    let api = fs::read_to_string(out_dir.join("src/generated/api-client.ts")).unwrap();
    assert!(api.contains("listEmployees"));
    // Task 4: client hits the versioned forge API prefix.
    assert!(api.contains("FORGE_API_PREFIX = \"/api/v1/forge\""));
    assert!(api.contains("${FORGE_API_PREFIX}/schemas/Employee/entities"));
    // Task 4: updates go through PATCH, not PUT.
    assert!(api.contains("method: \"PATCH\""));
    assert!(!api.contains("method: \"PUT\""));
    // Task 4: the client threads the Bearer token through tokenStore.
    assert!(api.contains("tokenStore.get()"));
    assert!(api.contains("Bearer ${token}"));
    // GH #36: rawEntityList opts out of the parallel COUNT(*) query, and
    // listQuery forwards `count: false` when set.
    assert!(api.contains("count?: boolean"));
    assert!(api.contains("count === false"));
    assert!(api.contains("count: false"));

    // Task 4: auth.ts exposes the expected surface.
    let auth_ts = fs::read_to_string(out_dir.join("src/lib/auth.ts")).unwrap();
    assert!(auth_ts.contains("/api/v1/forge/auth/login"));
    assert!(auth_ts.contains("export const tokenStore"));
    assert!(auth_ts.contains("export function isAuthenticated"));

    // Task 4 + Phase 2: App.tsx wires RequireAuth, /login, and /app + /admin
    // subtrees.
    let app_tsx = fs::read_to_string(out_dir.join("src/App.tsx")).unwrap();
    assert!(app_tsx.contains("<RequireAuth>"));
    assert!(app_tsx.contains("path=\"/login\""));
    assert!(app_tsx.contains("/app/${r.path}"));
    assert!(app_tsx.contains("path=\"/admin/*\""));
    assert!(app_tsx.contains("AdminLayout"));

    // Phase 2: route-manifest imports from @/app/pages and emits mount-relative
    // paths (no leading /app — that is added by App.tsx).
    let manifest = fs::read_to_string(out_dir.join("src/generated/route-manifest.ts")).unwrap();
    assert!(manifest.contains("@/app/pages/employee/list"));
    assert!(manifest.contains("path: \"employee\""));
    assert!(!manifest.contains("path: \"/employee\""));
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

    let list_path = out_dir.join("src/app/pages/employee/list.tsx");
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
        err.contains("@system") || err.contains("system schema"),
        "stderr: {err}"
    );
}

#[test]
fn templates_dir_override_wins_over_embedded() {
    // --templates-dir lets users iterate on generator output without a CLI
    // rebuild. Override files shadow embedded templates one-for-one; any
    // template not present in the override dir still comes from the binary,
    // so a one-file override must not break the rest of the tree.
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    let overrides = tmp.path().join("tpl-overrides");
    write_schemas(&schema_dir, V0_EMPLOYEE);

    // Override a single leaf template with a trivial marker payload. index.html
    // is Owned mode, so the generator will write it verbatim and we can read
    // it back to prove the override path fired.
    let sentinel = "<!-- OVERRIDE-SENTINEL-7f3a -->";
    fs::create_dir_all(&overrides).unwrap();
    fs::write(
        overrides.join("index.html.jinja"),
        format!("<!doctype html>\n{sentinel}\n<html><body></body></html>\n"),
    )
    .unwrap();

    run_generate(
        &schema_dir,
        &out_dir,
        "Employee",
        &["--templates-dir", overrides.to_str().unwrap()],
    )
    .assert()
    .success();

    let index = fs::read_to_string(out_dir.join("index.html")).unwrap();
    assert!(
        index.contains(sentinel),
        "override index.html should include sentinel, got:\n{index}"
    );

    // Non-overridden templates still come from the embedded defaults — the
    // package.json should be untouched.
    let pkg = fs::read_to_string(out_dir.join("package.json")).unwrap();
    assert!(
        pkg.contains("\"react\""),
        "embedded package.json still applies"
    );

    // And a Rust-side App.tsx should still contain the generated nav wiring.
    let app = fs::read_to_string(out_dir.join("src/App.tsx")).unwrap();
    assert!(
        app.contains("routeManifest"),
        "embedded App.tsx should still be rendered"
    );
}

/// Issue #35: a `-> X[]` field paired against a child FK is a derived
/// inverse collection. The backend rejects writes with 422, so the site
/// generator must omit derived fields from the create/edit form and its
/// zod schema. The detail view keeps rendering them as a linked list via
/// the existing relation-display path.
const DERIVED_PAIR: &str = r#"
@display("title")
schema Opportunity {
    title: text(max: 255) required
    documents: -> Document[]
}

@display("name")
schema Document {
    name: text(max: 255) required
    opportunity: -> Opportunity
}
"#;

#[test]
fn derived_inverse_collection_skipped_in_forms() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, DERIVED_PAIR);

    run_generate(&schema_dir, &out_dir, "Opportunity", &[])
        .assert()
        .success();

    // Zod schema for Opportunity must NOT include the derived `documents`
    // field — typing into it on the form would 422 at submit.
    let zod = fs::read_to_string(out_dir.join("src/generated/zod-schemas.ts")).unwrap();
    let opp_block_start = zod
        .find("opportunitySchema")
        .expect("opportunitySchema must be generated");
    let opp_block_end = zod[opp_block_start..]
        .find("})")
        .map(|i| opp_block_start + i)
        .unwrap_or(zod.len());
    let opp_block = &zod[opp_block_start..opp_block_end];
    assert!(
        !opp_block.contains("documents:"),
        "derived field `documents` must not appear in opportunitySchema:\n{opp_block}"
    );

    // Edit template must not render a FormField for `documents`.
    let edit = fs::read_to_string(out_dir.join("src/app/pages/opportunity/edit.tsx")).unwrap();
    assert!(
        !edit.contains("name=\"documents\""),
        "edit form should not render a control for derived field `documents`"
    );
    // The non-derived child-side FK on Document still wires up its edit
    // control — sanity check that we didn't over-filter.
    run_generate(&schema_dir, &out_dir, "Document", &[])
        .assert()
        .success();
    let doc_edit =
        fs::read_to_string(out_dir.join("src/app/pages/document/edit.tsx")).unwrap();
    assert!(
        doc_edit.contains("name=\"opportunity\""),
        "child-side FK should still render as an editable relation_one control"
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
