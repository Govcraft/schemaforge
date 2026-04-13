use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let descriptor_path = out_dir.join("translation_hooks.bin");

    tonic_prost_build::configure()
        .file_descriptor_set_path(&descriptor_path)
        .build_server(true)
        .build_client(false)
        .out_dir(&out_dir)
        .compile_protos(&["tests/proto/translation_hooks.proto"], &["tests/proto"])?;

    println!("cargo:rerun-if-changed=tests/proto/translation_hooks.proto");
    println!(
        "cargo:rustc-env=TRANSLATION_HOOKS_DESCRIPTOR={}",
        descriptor_path.display()
    );

    Ok(())
}
