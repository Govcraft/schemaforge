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
