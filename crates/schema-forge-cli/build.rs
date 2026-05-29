//! Embed every `templates/site/**/*.jinja` file into the CLI binary.
//!
//! Walks `templates/site/` at build time and emits
//! `$OUT_DIR/embedded_site_templates.rs`, a `const EMBEDDED_SITE_TEMPLATES`
//! slice of `(logical_name, file_contents)` pairs consumed by
//! `commands::site::render`. Logical names strip the `.jinja` suffix and use
//! forward slashes (e.g. `src/App.tsx.jinja` → `src/App.tsx`), matching the
//! names used at render call sites.
//!
//! Adding a new template is purely a filesystem operation: drop a new
//! `.jinja` file under `templates/site/` and rebuild.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let templates_root = manifest_dir.join("templates").join("site");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("embedded_site_templates.rs");

    println!("cargo:rerun-if-changed={}", templates_root.display());

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    walk(&templates_root, &templates_root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    out.push_str("pub const EMBEDDED_SITE_TEMPLATES: &[(&str, &str)] = &[\n");
    for (logical, abs) in &entries {
        out.push_str(&format!(
            "    (\"{}\", include_str!(r\"{}\")),\n",
            logical,
            abs.display()
        ));
    }
    out.push_str("];\n");

    fs::write(&dest, out).unwrap();

    resolve_console_dist(&out_dir);
}

/// Resolve the directory `rust-embed` reads the ops console from, for the
/// `embedded-console` feature (`commands/serve_console.rs`).
///
/// Only active under that feature (cargo exports `CARGO_FEATURE_EMBEDDED_CONSOLE`
/// for enabled features); otherwise the module is never compiled and this is a
/// no-op. Resolution:
///   - `SCHEMAFORGE_CONSOLE_DIST` set -> use it (release CI points this at the
///     downloaded, signature-verified console `dist`; dev points it at a local
///     `apps/console/dist`). Must contain `index.html`.
///   - unset -> write a tiny stub `index.html` into `OUT_DIR` so the feature
///     still compiles locally without a bundle present.
///
/// The resolved path is re-exported via `cargo:rustc-env` so the
/// `#[folder = "$SCHEMAFORGE_CONSOLE_DIST"]` attribute on `ConsoleAssets`
/// (rust-embed `interpolate-folder-path`) expands to it during macro expansion.
fn resolve_console_dist(out_dir: &Path) {
    println!("cargo:rerun-if-env-changed=SCHEMAFORGE_CONSOLE_DIST");
    if env::var_os("CARGO_FEATURE_EMBEDDED_CONSOLE").is_none() {
        return;
    }

    let dist = match env::var_os("SCHEMAFORGE_CONSOLE_DIST").filter(|p| !p.is_empty()) {
        Some(p) => {
            let path = PathBuf::from(&p);
            assert!(
                path.join("index.html").is_file(),
                "SCHEMAFORGE_CONSOLE_DIST={} has no index.html; point it at a built console `dist`",
                path.display()
            );
            println!("cargo:rerun-if-changed={}", path.display());
            // Absolute so rust-embed resolves it independently of CWD.
            fs::canonicalize(&path).unwrap_or(path)
        }
        None => write_console_stub(out_dir),
    };

    println!(
        "cargo:rustc-env=SCHEMAFORGE_CONSOLE_DIST={}",
        dist.display()
    );
}

/// Write a placeholder `dist` into `OUT_DIR` so `--features embedded-console`
/// compiles even when no bundle was supplied. A binary built this way serves a
/// short "console not bundled" page instead of the real app.
fn write_console_stub(out_dir: &Path) -> PathBuf {
    let stub = out_dir.join("console-stub");
    fs::create_dir_all(&stub).expect("create console stub dir");
    fs::write(
        stub.join("index.html"),
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <title>SchemaForge</title></head><body>\
         <p>The ops console was not bundled into this build. Rebuild with \
         <code>--features embedded-console</code> and <code>SCHEMAFORGE_CONSOLE_DIST</code> \
         pointed at a built console <code>dist</code>.</p></body></html>",
    )
    .expect("write stub index.html");
    stub
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            walk(root, &path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jinja") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let rel = path.strip_prefix(root).unwrap();
        let without_jinja = rel.with_extension("");
        let logical = without_jinja
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        out.push((logical, path));
    }
}
