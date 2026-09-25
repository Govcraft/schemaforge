//! Product identity remains configurable across regeneration and drift checks.
use std::fs;
use std::path::Path;

use assert_cmd::{cargo_bin_cmd, Command};
use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("schemas")).unwrap();
    fs::write(
        dir.path().join("schemas/test.schema"),
        "schema Invoice { number: text required }",
    )
    .unwrap();
    dir
}

fn generate(root: &Path) -> Command {
    let mut command = cargo_bin_cmd!("schemaforge");
    command
        .current_dir(root)
        .args(["site", "generate", "-s", "schemas", "-o", "site"]);
    command
}

fn read(root: &Path, file: &str) -> String {
    fs::read_to_string(root.join("site").join(file)).unwrap()
}

#[test]
fn configured_branding_survives_generation_and_check() {
    let dir = fixture();
    let root = dir.path();
    fs::create_dir(root.join("branding")).unwrap();
    for (name, color) in [("logo", "red"), ("dark", "white"), ("icon", "blue")] {
        fs::write(root.join(format!("branding/{name}.svg")), format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="32" height="32" fill="{color}"/></svg>"#)).unwrap();
    }
    fs::write(
        root.join("config.toml"),
        r#"
[schema_forge.site]
name = 'Acme "Operations" & <Review>'
title_suffix = ""
logo = "branding/logo.svg"
logo_on_dark = "branding/dark.svg"
favicon = "branding/icon.svg"
"#,
    )
    .unwrap();
    generate(root).assert().success();
    let package: serde_json::Value = serde_json::from_str(&read(root, "package.json")).unwrap();
    assert_eq!(package["name"], "acme-operations-review");
    let branding = read(root, "src/lib/branding.ts");
    assert!(branding.contains(r#""Acme \"Operations\" & <Review>""#));
    assert!(branding.contains("TITLE_SUFFIX: string = \"\""));
    let html = read(root, "index.html");
    assert!(html.contains("&lt;Review&gt;"));
    assert!(html.contains("/favicon.svg"));
    for (output, color) in [
        ("logo-mark", "red"),
        ("logo-mark-white", "white"),
        ("favicon", "blue"),
    ] {
        assert!(read(root, &format!("public/{output}.svg")).contains(&format!("fill=\"{color}\"")));
    }
    generate(root).arg("--check").assert().success();
    generate(root)
        .args(["--name", "Other Product", "--title-suffix", "Acme"])
        .assert()
        .success();
    assert!(read(root, "src/lib/branding.ts").contains("Other Product"));
    assert!(read(root, "src/pages/login.tsx").contains("{SITE_NAME}"));
    generate(root)
        .args([
            "--name",
            "Other Product",
            "--title-suffix",
            "Acme",
            "--check",
        ])
        .assert()
        .success();
    fs::write(root.join("site/public/logo-mark.svg"), "changed").unwrap();
    generate(root).arg("--check").assert().failure();
}

#[test]
fn vendor_outputs_accept_template_overrides_and_defaults_are_neutral() {
    let dir = fixture();
    let root = dir.path();
    generate(root).assert().success();
    for file in [
        "public/logo-mark.svg",
        "public/logo-mark-white.svg",
        "src/index.css",
        "src/pages/login.tsx",
        "src/pages/accessibility.tsx",
        "src/lib/use-document-title.ts",
    ] {
        let contents = read(root, file);
        assert!(!contents.contains("Govcraft"), "{file}");
        assert!(!contents.contains("· SchemaForge"), "{file}");
    }
    let overrides = root.join("custom");
    for (file, content) in [
        ("public/logo-mark-white.svg", "<svg>custom dark mark</svg>"),
        ("public/favicon.svg", "<svg>custom favicon</svg>"),
        (
            "src/index.css",
            "/* custom styles for {{ branding.name }} */",
        ),
        ("src/lib/use-document-title.ts", "// custom title hook"),
    ] {
        let path = overrides.join(format!("{file}.jinja"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    generate(root)
        .args(["--templates-dir", "custom", "--name", "Acme"])
        .assert()
        .success();
    assert!(read(root, "public/logo-mark-white.svg").contains("custom dark mark"));
    assert!(read(root, "public/favicon.svg").contains("custom favicon"));
    assert!(read(root, "src/index.css").contains("custom styles for Acme"));
    assert!(read(root, "src/lib/use-document-title.ts").contains("custom title hook"));
    generate(root)
        .args(["--templates-dir", "custom", "--name", "Acme", "--check"])
        .assert()
        .success();
}

#[test]
fn tenant_root_supplies_name_independent_of_output_directory() {
    let dir = fixture();
    fs::write(
        dir.path().join("schemas/test.schema"),
        "@tenant(root) schema AcmeOrganization { name: text }",
    )
    .unwrap();
    generate(dir.path()).assert().success();
    assert!(read(dir.path(), "src/lib/branding.ts").contains("Acme Organization"));
}

#[test]
fn unsupported_assets_and_blank_names_fail_clearly() {
    let dir = fixture();
    fs::write(dir.path().join("logo.png"), b"PNG").unwrap();
    generate(dir.path())
        .args(["--logo", "logo.png"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("must be an SVG file"));
    generate(dir.path())
        .args(["--name", " "])
        .assert()
        .failure()
        .stderr(predicates::str::contains("site name must be nonempty"));
}

#[test]
fn config_relative_assets_and_flag_overrides_use_their_own_bases() {
    let dir = fixture();
    let root = dir.path();
    fs::create_dir(root.join("settings")).unwrap();
    fs::write(
        root.join("settings/config.toml"),
        "[schema_forge.site]\nname = 'Configured'\nlogo = 'brand.svg'\n",
    )
    .unwrap();
    fs::write(
        root.join("settings/brand.svg"),
        "<svg>configured mark</svg>",
    )
    .unwrap();
    fs::write(root.join("override.svg"), "<svg>flag mark</svg>").unwrap();
    generate(root)
        .args(["--config", "settings/config.toml"])
        .assert()
        .success();
    assert!(read(root, "public/logo-mark.svg").contains("configured mark"));
    generate(root)
        .args(["--config", "settings/config.toml", "--logo", "override.svg"])
        .assert()
        .success();
    assert!(read(root, "public/logo-mark.svg").contains("flag mark"));
    assert!(read(root, "public/logo-mark-white.svg").contains("flag mark"));
    assert!(read(root, "public/favicon.svg").contains("flag mark"));
}
