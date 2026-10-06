//! Resolve optional embedded server console assets.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=SCHEMAFORGE_SOURCE_REVISION");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo output directory"));
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
