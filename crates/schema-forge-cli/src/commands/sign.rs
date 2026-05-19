//! `schemaforge sign` — produce per-file signatures and a signed
//! directory manifest using the operator's chosen scheme.

use std::path::PathBuf;

use schema_forge_signing::{
    sign_directory, CosignKeylessSigner, DirectorySigner, Ed25519Signer, SignReport, SshSigner,
};

use crate::cli::{GlobalOpts, SignArgs};
use crate::config::{load_svc_config, resolve_signing_config};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// One of the concrete signers the CLI knows how to build. Carrying the
/// concrete type (rather than `Box<dyn DirectorySigner>`) lets the
/// `--print-pubkey` flow surface scheme-specific advice without a
/// separate dispatch table. Both variants are boxed so the enum's
/// stack footprint stays small and the variants stay close in size
/// (clippy::large_enum_variant). The CLI only constructs one `CliSigner`
/// per invocation, so the heap allocations are immaterial.
enum CliSigner {
    Ed25519(Box<Ed25519Signer>),
    Ssh(Box<SshSigner>),
    Cosign(Box<CosignKeylessSigner>),
}

impl DirectorySigner for CliSigner {
    fn sign_to_sig_bytes(
        &self,
        message: &[u8],
    ) -> Result<Vec<u8>, schema_forge_signing::SigningError> {
        match self {
            CliSigner::Ed25519(s) => s.sign_to_sig_bytes(message),
            CliSigner::Ssh(s) => s.sign_to_sig_bytes(message),
            CliSigner::Cosign(s) => s.sign_to_sig_bytes(message),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            CliSigner::Ed25519(s) => s.kind(),
            CliSigner::Ssh(s) => s.kind(),
            CliSigner::Cosign(s) => s.kind(),
        }
    }
}

/// Run the `sign` command.
pub async fn run(
    args: SignArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let manifest_filename = resolve_manifest_filename(&args, global)?;
    let (manifest_dir, files) = resolve_inputs(&args.paths)?;

    let signer = build_signer(&args, output)?;

    let report = sign_directory(
        &manifest_dir,
        &files,
        &signer,
        manifest_filename.as_deref(),
    )?;

    if args.print_pubkey {
        emit_pubkey_advice(&signer, args.ssh_principal.as_deref(), output)?;
    }

    emit_report(output, &report);
    Ok(())
}

/// Either the explicit `--manifest-filename` flag or the operator's
/// `[schema_forge.signing] manifest_filename` setting. Keeping these
/// two paths in sync at sign time avoids a class of "I signed
/// `manifest.toml` but my verifier looks for `schemas.manifest.toml`"
/// failures.
fn resolve_manifest_filename(
    args: &SignArgs,
    global: &GlobalOpts,
) -> Result<Option<String>, CliError> {
    if let Some(name) = &args.manifest_filename {
        return Ok(Some(name.clone()));
    }
    let svc = load_svc_config(global)?;
    let signing = resolve_signing_config(&svc, global)?;
    Ok(signing.manifest_filename)
}

/// Collapse CLI paths into `(manifest_dir, file_list)`.
///
/// One directory → use that dir as the manifest root and recursively
/// gather every `.schema` inside.
///
/// Multiple paths or any file path → derive the shared parent, fail
/// if the inputs don't share one. Refusing mixed inputs is the
/// conservative choice; a single command writing two unrelated
/// manifests would be a surprise.
pub(crate) fn resolve_inputs_for_verify(
    paths: &[PathBuf],
) -> Result<(PathBuf, Vec<PathBuf>), CliError> {
    resolve_inputs(paths)
}

fn resolve_inputs(paths: &[PathBuf]) -> Result<(PathBuf, Vec<PathBuf>), CliError> {
    if paths.is_empty() {
        return Err(CliError::NoSchemaFiles {
            path: PathBuf::from("schemas/"),
        });
    }

    if paths.len() == 1 && paths[0].is_dir() {
        let manifest_dir = paths[0].clone();
        let files = discover_schema_files(std::slice::from_ref(&manifest_dir))?;
        return Ok((manifest_dir, files));
    }

    // Everything must be a file (or several dirs we cannot reduce).
    let mut files = Vec::new();
    let mut parents: Vec<PathBuf> = Vec::new();
    for p in paths {
        if !p.is_file() {
            return Err(CliError::Other(format!(
                "{} is not a file; pass a single directory or a list of .schema files",
                p.display()
            )));
        }
        files.push(p.clone());
        parents.push(p.parent().unwrap_or(p).to_path_buf());
    }
    let first = parents.first().cloned().unwrap_or_default();
    if !parents.iter().all(|p| p == &first) {
        return Err(CliError::Other(
            "all schema inputs must share a single parent directory containing the manifest".into(),
        ));
    }
    Ok((first, files))
}

/// Reuse `discover_schema_files` semantics without making the parse
/// helper public. The duplication is tiny and avoids leaking the
/// glob crate further than necessary.
fn discover_schema_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
        } else if path.is_dir() {
            let pattern = format!("{}/**/*.schema", path.display());
            let entries = glob::glob(&pattern).map_err(|e| CliError::Other(e.to_string()))?;
            for entry in entries {
                let entry = entry.map_err(|e| CliError::Other(e.to_string()))?;
                files.push(entry);
            }
        } else {
            return Err(CliError::NoSchemaFiles { path: path.clone() });
        }
    }
    if files.is_empty() {
        return Err(CliError::NoSchemaFiles {
            path: paths.first().cloned().unwrap_or_default(),
        });
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn build_signer(args: &SignArgs, output: &OutputContext) -> Result<CliSigner, CliError> {
    let key = &args.key;
    if let Some(path) = &key.ed25519_key {
        return Ed25519Signer::from_pem_file(path)
            .map(|s| CliSigner::Ed25519(Box::new(s)))
            .map_err(CliError::from);
    }
    if let Some(path) = &key.ed25519_generate {
        if path.exists() && !args.force {
            return Err(CliError::Other(format!(
                "{} already exists; pass --force to overwrite",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| CliError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
        }
        let signer = Ed25519Signer::from_seed_bytes(&seed_from_os_rng());
        signer.write_keypair(path)?;
        output.status(&format!("generated keypair at {}", path.display()));
        return Ok(CliSigner::Ed25519(Box::new(signer)));
    }
    if let Some(path) = &key.ssh_key {
        return SshSigner::from_openssh_file(path)
            .map(|s| CliSigner::Ssh(Box::new(s)))
            .map_err(CliError::from);
    }
    if key.keyless {
        // Probe `cosign version` at signer construction so a missing
        // or broken cosign binary fails immediately, not after we've
        // started writing per-file signatures.
        let signer = CosignKeylessSigner::try_new(&args.cosign_bin).map_err(CliError::from)?;
        output.status(&format!(
            "using cosign binary `{}` for keyless signing",
            signer.cosign_bin()
        ));
        return Ok(CliSigner::Cosign(Box::new(signer)));
    }
    Err(CliError::Other(
        "no signing scheme selected; pass --ed25519-key, --ed25519-generate, --ssh-key, or --keyless".into(),
    ))
}

/// Reads 32 bytes from the OS RNG. ed25519-dalek 2.x couples its
/// `rand_core` feature to an older `RngCore` ABI that has churn we
/// don't want to track; pulling raw bytes from `getrandom` (which
/// dalek already depends on transitively via `tempfile`'s feature)
/// avoids the version bind.
fn seed_from_os_rng() -> [u8; 32] {
    use std::io::Read;
    let mut buf = [0u8; 32];
    // /dev/urandom is the canonical entropy source on Linux. On
    // non-Linux Unix and macOS it exists too. Windows isn't a target
    // platform for production SchemaForge builds today (the FIPS
    // requirements mean Linux + AWS-LC), so the simple read is fine.
    let mut f = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
    f.read_exact(&mut buf).expect("read 32 bytes from /dev/urandom");
    buf
}

/// Scheme-aware advice for `--print-pubkey`. Operators copy the
/// printed block straight into their trust policy; the goal is that
/// every supported scheme has a copy-pasteable equivalent.
fn emit_pubkey_advice(
    signer: &CliSigner,
    ssh_principal: Option<&str>,
    output: &OutputContext,
) -> Result<(), CliError> {
    match signer {
        CliSigner::Ed25519(s) => {
            let pubkey_b64 = s.public_key_b64_spki()?;
            output.status(&format!("public key (base64 SPKI): {pubkey_b64}"));
            output.status(
                "  paste this into your trust policy:\n\
                 \n  [[schema_forge.signing.trusted_signers]]\n  kind = \"ed25519\"\n  \
                 name = \"<give-me-a-name>\"\n  public_key_b64 = \"<above>\"",
            );
        }
        CliSigner::Ssh(s) => {
            let line = s.allowed_signers_line(ssh_principal)?;
            output.status(&format!("public key (allowed_signers line): {line}"));
            output.status(
                "  add the line above to an allowed_signers file, then reference it:\n\
                 \n  [[schema_forge.signing.trusted_signers]]\n  kind = \"ssh-allowed-signers\"\n  \
                 name = \"<give-me-a-name>\"\n  path = \"/etc/schemaforge/allowed_signers\"\n\n  \
                 SchemaForge signs under namespace 'schema-forge-signing@govcraft.ai'; \
                 namespace-restricted entries must include that string.",
            );
        }
        CliSigner::Cosign(_) => {
            // Keyless signing has no static public key to advertise —
            // the Fulcio cert in each `.sig` bundle is ephemeral.
            // What stays stable across signings is the OIDC identity,
            // so we point the operator at the matching trust-anchor
            // shape instead.
            output.status(
                "cosign-keyless leaves no static public key to print — each `.sig` carries \
                 its own short-lived Fulcio certificate. Configure the trust anchor by \
                 pinning the OIDC issuer and a subject glob that matches your signing \
                 identity:\n\
                 \n  [[schema_forge.signing.trusted_signers]]\n  kind = \"cosign-keyless\"\n  \
                 name = \"<give-me-a-name>\"\n  issuer = \"https://token.actions.githubusercontent.com\"\n  \
                 subject_pattern = \"https://github.com/<org>/<repo>/.github/workflows/<wf>.yml@refs/tags/v*\"\n\n  \
                 For other issuers (Google, GitHub OAuth, custom OIDC), substitute the \
                 issuer URL and the subject your token vendor sets as the SAN.",
            );
        }
    }
    Ok(())
}

fn emit_report(output: &OutputContext, report: &SignReport) {
    match output.mode {
        OutputMode::Human => {
            output.success(&format!(
                "signed {} schemas; manifest at {}",
                report.signed.len(),
                report.manifest_path.display()
            ));
            for s in &report.signed {
                output.status(&format!(
                    "  {} -> {}",
                    s.schema_path.display(),
                    s.signature_path.display()
                ));
            }
        }
        OutputMode::Json => {
            let json = serde_json::json!({
                "manifest_path": report.manifest_path.display().to_string(),
                "manifest_signature_path": report.manifest_signature_path.display().to_string(),
                "signed": report.signed.iter().map(|s| serde_json::json!({
                    "schema_path": s.schema_path.display().to_string(),
                    "signature_path": s.signature_path.display().to_string(),
                })).collect::<Vec<_>>(),
            });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            for s in &report.signed {
                println!("{}\t{}", s.schema_path.display(), s.signature_path.display());
            }
            println!("manifest\t{}", report.manifest_path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolve_inputs_accepts_single_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.schema"), b"A").unwrap();
        let (root, files) = resolve_inputs(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(root, dir.path());
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn resolve_inputs_accepts_file_list_with_shared_parent() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.schema");
        let b = dir.path().join("b.schema");
        std::fs::write(&a, b"A").unwrap();
        std::fs::write(&b, b"B").unwrap();
        let (root, files) = resolve_inputs(&[a, b]).unwrap();
        assert_eq!(root, dir.path());
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn resolve_inputs_rejects_split_parents() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        let fa = a.path().join("a.schema");
        let fb = b.path().join("b.schema");
        std::fs::write(&fa, b"A").unwrap();
        std::fs::write(&fb, b"B").unwrap();
        let err = resolve_inputs(&[fa, fb]).unwrap_err();
        assert!(matches!(err, CliError::Other(_)));
    }
}
