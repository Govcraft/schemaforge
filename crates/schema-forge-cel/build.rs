//! Build script for the CEL conformance oracle (#90).
//!
//! Two jobs:
//! 1. Generate Rust types for the vendored cel-spec conformance protos, so the
//!    test harness can decode the corpus. The generated code is consumed only by
//!    the integration test, never by the engine itself.
//! 2. Pre-encode each vendored `*.textproto` to a binary `*.binpb` via `protoc`,
//!    so the test binary never has to shell out to `protoc` at runtime.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    // 1. Generate decode types for the conformance protos.
    prost_build::Config::new()
        .include_file("_includes.rs")
        .compile_protos(
            &["proto/cel/expr/conformance/test/simple.proto"],
            &["proto"],
        )?;

    // 2. Pre-encode the subset corpus: textproto -> binary.
    //
    // Some subset files embed a `google.protobuf.Any` of a conformance test type
    // (`proto2/proto3.TestAllTypes`). protoc must have those types in its
    // descriptor pool to parse the text, so we hand it every vendored proto. The
    // engine never sees these types — `convert_value` skips `Any` at run time.
    let protoc = env::var("PROTOC").unwrap_or_else(|_| "protoc".to_string());
    let binpb_dir = out_dir.join("binpb");
    fs::create_dir_all(&binpb_dir)?;

    let mut all_protos = Vec::new();
    collect_protos(&PathBuf::from("proto"), &mut all_protos)?;
    all_protos.sort();

    let mut textprotos: Vec<PathBuf> = fs::read_dir("testdata/simple")?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("textproto"))
        .collect();
    textprotos.sort();

    for path in &textprotos {
        let text = fs::read(path)?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("non-utf8 testdata filename")?;

        let mut child = Command::new(&protoc)
            .arg("--encode=cel.expr.conformance.test.SimpleTestFile")
            .arg("-Iproto")
            .args(&all_protos)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("failed to open protoc stdin")?
            .write_all(&text)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(format!(
                "protoc --encode failed for {}:\n{}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        fs::write(binpb_dir.join(format!("{stem}.binpb")), &output.stdout)?;
    }

    println!(
        "cargo:rustc-env=CEL_CONFORMANCE_BINPB={}",
        binpb_dir.display()
    );
    println!("cargo:rerun-if-changed=proto");
    println!("cargo:rerun-if-changed=testdata/simple");
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}

/// Recursively collect every `*.proto` under `dir`.
fn collect_protos(dir: &PathBuf, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_protos(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("proto") {
            out.push(path);
        }
    }
    Ok(())
}
