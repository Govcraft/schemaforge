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
    // Each page is split into a Preserve shell (`.tsx`) and an Owned
    // schema-driven sibling (`.generated.tsx`) — see issue #40.
    for f in [
        "src/pages/login.tsx",
        "src/app/pages/employee/list.tsx",
        "src/app/pages/employee/list.generated.tsx",
        "src/app/pages/employee/detail.tsx",
        "src/app/pages/employee/detail.generated.tsx",
        "src/app/pages/employee/edit.tsx",
        "src/app/pages/employee/edit.generated.tsx",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }

    // The runtime-dynamic admin shell was moved to the schemaforge-console
    // repo. The generator must no longer emit a src/admin/ tree.
    assert!(
        !out_dir.join("src/admin").exists(),
        "src/admin/ should not be generated — the admin console moved to schemaforge-console",
    );

    // Spot-check template substitutions
    let api = fs::read_to_string(out_dir.join("src/generated/api-client.ts")).unwrap();
    assert!(api.contains("listEmployees"));
    // Task 4: client hits the versioned forge API prefix.
    assert!(api.contains("FORGE_API_PREFIX = \"/api/v1/forge\""));
    assert!(api.contains("${FORGE_API_PREFIX}/schemas/Employee/entities"));
    // Task 4: updates go through PATCH, not PUT.
    assert!(api.contains("method: \"PATCH\""));
    assert!(!api.contains("method: \"PUT\""));
    // Requests share the current bearer token and active tenant headers.
    assert!(api.contains("authenticatedHeaders(init?.headers)"));
    let auth = fs::read_to_string(out_dir.join("src/lib/auth.ts")).unwrap();
    assert!(auth.contains("const token = tokenStore.get()"));
    assert!(auth.contains("Bearer ${token}"));
    assert!(auth.contains("headers.set(ACTIVE_TENANT_HEADER, active)"));
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

    // App.tsx wires RequireAuth, /login, and the /app subtree. The /admin
    // shell is gone; the home route redirects to the first app entity.
    let app_tsx = fs::read_to_string(out_dir.join("src/App.tsx")).unwrap();
    assert!(app_tsx.contains("<RequireAuth>"));
    assert!(app_tsx.contains("path=\"/login\""));
    assert!(app_tsx.contains("/app/${r.path}"));
    assert!(app_tsx.contains("/app/${defaultEntity}"));
    assert!(
        !app_tsx.contains("/admin"),
        "App.tsx must not reference the removed /admin shell"
    );
    assert!(!app_tsx.contains("AdminLayout"));

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
fn custom_error_handlers_survive_regeneration_and_drift_check() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(&schema_dir, V0_EMPLOYEE);
    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();
    let handler = out_dir.join("src/lib/error-toast.ts");
    let scaffold = fs::read_to_string(&handler).unwrap();
    assert!(scaffold.contains("suppressGlobalError"));
    let customization = "export function onQueryError() {}\nexport function onMutationError() {}\n";
    fs::write(&handler, customization).unwrap();
    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();
    run_generate(&schema_dir, &out_dir, "Employee", &["--check"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(handler).unwrap(), customization);
}

#[test]
fn issue_40_preserve_shell_survives_while_generated_refreshes() {
    // Acceptance criterion from #40: schema changes flow into the
    // per-entity pages without touching user-owned layout or composition.
    // The Preserve `.tsx` shell must stay byte-identical, while the Owned
    // `.generated.tsx` sibling picks up the new schema shape on every run.
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");

    // Round 1: generate from a simple schema.
    let schema_v1 = r#"
@display("name")
schema Widget {
    name: text(max: 255) required
}
"#;
    write_schemas(&schema_dir, schema_v1);
    run_generate(&schema_dir, &out_dir, "Widget", &[])
        .assert()
        .success();

    // User takes ownership of the preserve shell. This simulates the
    // real pain point in #40 — dropping a chart or custom state into
    // `list.tsx` and expecting it to survive future regens.
    let list_shell = out_dir.join("src/app/pages/widget/list.tsx");
    let user_content = "// bespoke composition owned by the user\n";
    fs::write(&list_shell, user_content).unwrap();

    // Round 2: schema grows a new column.
    let schema_v2 = r#"
@display("name")
schema Widget {
    name: text(max: 255) required
    sku: text(max: 64) required
}
"#;
    write_schemas(&schema_dir, schema_v2);
    run_generate(&schema_dir, &out_dir, "Widget", &[])
        .assert()
        .success();

    // Preserve shell is untouched.
    assert_eq!(
        fs::read_to_string(&list_shell).unwrap(),
        user_content,
        "preserve shell must survive regen without --force-user-files",
    );

    // Generated sibling picked up the new column without any user action.
    let list_gen =
        fs::read_to_string(out_dir.join("src/app/pages/widget/list.generated.tsx")).unwrap();
    assert!(
        list_gen.contains("accessorKey: \"sku\""),
        "list.generated.tsx must refresh with new schema fields:\n{list_gen}"
    );

    // And the idempotence check still passes end-to-end.
    run_generate(&schema_dir, &out_dir, "Widget", &["--check"])
        .assert()
        .success();
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

    // Edit form schema must not render a FormField for `documents`. The
    // schema-driven form-field rendering lives in the Owned
    // `edit.generated.tsx` sibling now (see issue #40).
    let edit_gen =
        fs::read_to_string(out_dir.join("src/app/pages/opportunity/edit.generated.tsx")).unwrap();
    assert!(
        !edit_gen.contains("name=\"documents\""),
        "edit form should not render a control for derived field `documents`"
    );
    // The non-derived child-side FK on Document still wires up its edit
    // control — sanity check that we didn't over-filter.
    run_generate(&schema_dir, &out_dir, "Document", &[])
        .assert()
        .success();
    let doc_edit_gen =
        fs::read_to_string(out_dir.join("src/app/pages/document/edit.generated.tsx")).unwrap();
    assert!(
        doc_edit_gen.contains("name=\"opportunity\""),
        "child-side FK should still render as an editable relation_one control"
    );
}

/// Issue #43: the per-entity `.generated.tsx` siblings used to import
/// `Link` and declare a `form` parameter unconditionally, even when the
/// entity carried no display-field-bearing relations and no JSON fields.
/// That tripped TS's `noUnusedLocals` / `noUnusedParameters` (both enabled
/// in the scaffolded `tsconfig.json`) and broke `pnpm build` out of the
/// box. Both symbols must be conditionally emitted now.
#[test]
fn issue_43_no_unused_link_import_or_form_param_for_simple_entity() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    // Glossary has no relations and no JSON — both unused symbols would
    // have shown up here pre-fix.
    let schema = r#"
@display("term")
schema Glossary {
    term:       text(max: 255) required
    definition: text(max: 2048) required
}
"#;
    write_schemas(&schema_dir, schema);

    run_generate(&schema_dir, &out_dir, "Glossary", &[])
        .assert()
        .success();

    let detail =
        fs::read_to_string(out_dir.join("src/app/pages/glossary/detail.generated.tsx")).unwrap();
    assert!(
        !detail.contains("import { Link }"),
        "detail.generated.tsx must not import Link when no relation has a display field:\n{detail}"
    );

    let edit =
        fs::read_to_string(out_dir.join("src/app/pages/glossary/edit.generated.tsx")).unwrap();
    // Shared recursive normalization uses the callback on malformed JSON,
    // including nested fields, so the form parameter is always referenced.
    assert!(edit.contains("form.setError(name as never"));
    assert!(edit.contains("normalizeFormPayload(values as Record<string, unknown>"));

    // And the contrapositive: an entity *with* a JSON field still gets the
    // real `form` parameter so the JSON-parse setError branch compiles.
    let schema_with_json = r#"
@display("name")
schema Settings {
    name:    text(max: 255) required
    payload: json
}
"#;
    let schema_dir2 = tmp.path().join("schemas2");
    let out_dir2 = tmp.path().join("site2");
    write_schemas(&schema_dir2, schema_with_json);
    run_generate(&schema_dir2, &out_dir2, "Settings", &[])
        .assert()
        .success();
    let edit_with_json =
        fs::read_to_string(out_dir2.join("src/app/pages/settings/edit.generated.tsx")).unwrap();
    let payload_sig_json =
        slice_between(&edit_with_json, "normalizeSettingsPayload(", "): Record<")
            .expect("normalizeSettingsPayload signature must be present");
    assert!(
        payload_sig_json.contains("\n  form: UseFormReturn<"),
        "entity with a JSON field must keep the real `form` param:\n{payload_sig_json}"
    );
}

/// Return the substring of `haystack` between the first occurrence of `start`
/// (after it) and the next occurrence of `end`. Used by tests that want to
/// scope a `contains(...)` assertion to a single function signature without
/// false-positiving on similarly-named symbols elsewhere in the same file.
fn slice_between<'a>(haystack: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = haystack.find(start)? + start.len();
    let j = haystack[i..].find(end)? + i;
    Some(&haystack[i..j])
}

/// Issue #43 (companion): an entity that *does* have a relation with a
/// display field still imports `Link`. Locks in the contrapositive of the
/// fix so a future refactor doesn't accidentally suppress the import in
/// every case.
#[test]
fn issue_43_relation_with_display_field_keeps_link_import() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    let schema = r#"
@display("name")
schema Department {
    name: text(max: 255) required
}

@display("full_name")
schema Employee {
    full_name:  text(max: 255) required
    department: -> Department
}
"#;
    write_schemas(&schema_dir, schema);
    run_generate(&schema_dir, &out_dir, "Employee", &[])
        .assert()
        .success();
    let detail =
        fs::read_to_string(out_dir.join("src/app/pages/employee/detail.generated.tsx")).unwrap();
    assert!(
        detail.contains("import { Link }"),
        "detail.generated.tsx must keep the Link import for relations with display fields:\n{detail}"
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

#[test]
fn invitations_registered_and_hidden_fields_omitted_from_forms() {
    let tmp = TempDir::new().unwrap();
    let schemas = tmp.path().join("schemas");
    let out = tmp.path().join("site");
    write_schemas(
        &schemas,
        r#"
        schema Profile {
            name: text required
            password_hash: text required @hidden
        }
    "#,
    );
    run_generate(&schemas, &out, "Profile", &[])
        .assert()
        .success();
    for file in [
        "src/pages/invite.tsx",
        "src/pages/invite-accept.tsx",
        "src/generated/invites.ts",
    ] {
        assert!(out.join(file).exists(), "missing {file}");
    }
    let routes = fs::read_to_string(out.join("src/generated/route-manifest.ts")).unwrap();
    assert!(routes.contains("/admin/users/invite"));
    assert!(routes.contains("/invite/accept"));
    for file in [
        "src/generated/zod-schemas.ts",
        "src/app/pages/profile/edit.generated.tsx",
        "src/app/pages/profile/detail.generated.tsx",
    ] {
        let content = fs::read_to_string(out.join(file)).unwrap();
        assert!(
            !content.contains("password_hash"),
            "hidden field leaked into {file}"
        );
        assert!(content.contains("name"));
    }
}

#[test]
fn invitation_tenant_picker_matches_annotation_wire_shape() {
    use schema_forge_core::types::{Annotation, TenantKind};
    let value = serde_json::to_value(Annotation::Tenant(TenantKind::Root)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "annotation": "Tenant", "Root": null })
    );
}

#[test]
fn generated_extended_fields_and_authority_survive_regeneration() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let out_dir = tmp.path().join("site");
    write_schemas(
        &schema_dir,
        r#"
    schema Job {
        name: text required
        timeout: duration required
        labels: map<text, integer>
        checksum: bytes
        delays: duration[]
        nested: composite { delay: duration data: map<text, integer> }
        computed: float @compute("1.0")
        secret: text @field_access(read: ["finance"], write: ["lead"])
    }
    "#,
    );
    run_generate(&schema_dir, &out_dir, "Job", &[])
        .assert()
        .success();
    let types = fs::read_to_string(out_dir.join("src/generated/entity-types.ts")).unwrap();
    for expected in [
        "timeout: string",
        "Record<string, number>",
        "checksum?: string",
        "delays?: string[]",
        "computed?: number",
    ] {
        assert!(types.contains(expected), "missing {expected}: {types}");
    }
    let edit = fs::read_to_string(out_dir.join("src/app/pages/job/edit.generated.tsx")).unwrap();
    for field in [
        "timeout",
        "labels",
        "checksum",
        "nested.delay",
        "nested.data",
    ] {
        assert!(
            edit.contains(&format!("name=\"{field}\"")),
            "missing control {field}"
        );
    }
    assert!(!edit.contains("name=\"computed\""));
    assert!(edit.contains("canWriteFormField([\"finance\"], [\"lead\"])"));
    assert!(edit.contains("normalizeFormPayload"));
    let zod = fs::read_to_string(out_dir.join("src/generated/zod-schemas.ts")).unwrap();
    let schema = zod.split("export const jobSchema").nth(1).unwrap();
    assert!(schema.contains("timeout: formFieldSchema(durationSchema"));
    assert!(!schema.contains("computed:"));
    assert!(schema.contains("jsonTextSchema(z.record(z.number().int()))"));
    let shell = out_dir.join("src/app/pages/job/edit.tsx");
    fs::write(&shell, "// user-owned edit shell\n").unwrap();
    run_generate(&schema_dir, &out_dir, "Job", &[])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(shell).unwrap(),
        "// user-owned edit shell\n"
    );
}

#[test]
fn relation_and_file_only_pages_emit_only_used_formatters() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    let fixtures = format!(
        "{}\n{}",
        include_str!("site_e2e/demo.schema"),
        include_str!("site_e2e/rendering.schema")
    );
    write_schemas(&schema_dir, &fixtures);
    for (name, path) in [
        ("PrimaryJoin", "primary-join"),
        ("HiddenJoin", "hidden-join"),
        ("ExplicitJoin", "explicit-join"),
        ("ManyJoin", "many-join"),
        ("FileOnly", "file-only"),
    ] {
        let out_dir = tmp.path().join(path);
        run_generate(&schema_dir, &out_dir, name, &[])
            .assert()
            .success();
        let detail =
            fs::read_to_string(out_dir.join(format!("src/app/pages/{path}/detail.generated.tsx")))
                .unwrap();
        let list =
            fs::read_to_string(out_dir.join(format!("src/app/pages/{path}/list.generated.tsx")))
                .unwrap();
        assert!(!detail.contains("formatFieldValue"), "{name}: {detail}");
        assert!(!detail.contains("function isEmpty"), "{name}: {detail}");
        assert!(!list.contains("formatFieldValue"), "{name}: {list}");
        if matches!(name, "PrimaryJoin" | "ExplicitJoin") {
            assert!(list.contains("row.original.company__display ?? row.original.company"));
            assert!(list.contains(&format!("/app/{path}/${{row.original.id}}")));
        }
        if name == "ManyJoin" {
            assert!(list.contains("row.original.companies__display?.[index] ?? id"));
        }
    }
}

#[test]
fn details_without_visible_values_keep_props_without_unused_bindings() {
    let tmp = TempDir::new().unwrap();
    let schema_dir = tmp.path().join("schemas");
    write_schemas(&schema_dir, "schema HiddenOnly { secret: text @hidden } schema HiddenComposite { details: composite { secret: text @hidden } }");
    for (name, path, has_rows) in [
        ("HiddenOnly", "hidden-only", false),
        ("HiddenComposite", "hidden-composite", true),
    ] {
        let out_dir = tmp.path().join(path);
        run_generate(&schema_dir, &out_dir, name, &[])
            .assert()
            .success();
        let detail =
            fs::read_to_string(out_dir.join(format!("src/app/pages/{path}/detail.generated.tsx")))
                .unwrap();
        assert!(
            detail.contains(&format!("DetailRows(_props: {{ data: {name} }}")),
            "{detail}"
        );
        assert_eq!(detail.contains("function specNum"), has_rows, "{detail}");
        assert!(!detail.contains("formatFieldValue"), "{detail}");
        assert!(!detail.contains("function isEmpty"), "{detail}");
    }
}
