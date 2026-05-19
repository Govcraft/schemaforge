//! `schemaforge trust-bundle` — manage the Sigstore TUF trust-root
//! snapshot the `cosign-keyless` verifier consumes.
//!
//! The airgap workflow this is built for:
//!
//! 1. On a connected host, run `schemaforge trust-bundle refresh
//!    --output trust_root.json`. The command fetches the latest
//!    Sigstore TUF metadata, verifies it against the embedded root of
//!    trust, and writes the resulting `trusted_root.json` payload to
//!    disk.
//! 2. Copy the file across the airgap into the deployment.
//! 3. Point `[schema_forge.signing] trust_root_bundle = ".../
//!    trust_root.json"` at it. Every cosign-keyless verifier in the
//!    policy now consults the operator-controlled snapshot instead of
//!    the trust root embedded in `sigstore-trust-root` (which is a
//!    frozen-in-time copy and will eventually age out as Fulcio
//!    rotates intermediates).
//!
//! `inspect` is a sanity helper: it parses a trust-root JSON and
//! prints a one-line summary, so the operator can confirm that the
//! file made it across the airgap intact and matches the expected
//! shape before deploying.

use std::path::Path;

use schema_forge_signing::SigningError;
use sigstore_trust_root::{SigstoreInstance, TrustedRoot};

use crate::cli::{GlobalOpts, TrustBundleInspectArgs, TrustBundleRefreshArgs};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Resolve the textual `--instance` flag onto the
/// `sigstore-trust-root` enum the TUF client takes. Reject anything
/// the clap value_parser would have rejected too — defensive double
/// check so a typo in `value_parser` doesn't silently fall through.
fn parse_instance(name: &str) -> Result<SigstoreInstance, CliError> {
    match name {
        "public-good" => Ok(SigstoreInstance::PublicGood),
        "staging" => Ok(SigstoreInstance::Staging),
        "github" => Ok(SigstoreInstance::GitHub),
        other => Err(CliError::Other(format!(
            "unknown Sigstore instance '{other}'; expected one of public-good, staging, github",
        ))),
    }
}

/// Run `schemaforge trust-bundle refresh`.
pub async fn run_refresh(
    args: TrustBundleRefreshArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    if args.output.exists() && !args.force {
        return Err(CliError::Other(format!(
            "{} already exists; pass --force to overwrite",
            args.output.display(),
        )));
    }

    let instance = parse_instance(&args.instance)?;
    output.status(&format!(
        "fetching trust root from Sigstore instance '{}' ...",
        args.instance,
    ));

    // `from_tuf` does the full TUF protocol dance: fetches metadata,
    // verifies signatures back to the embedded root, then fetches the
    // target file. Any failure here (network down, signature
    // mismatch, expired root) is fatal — we deliberately do NOT fall
    // back to the embedded snapshot, because the whole point of
    // `refresh` is to get a *current* root and a silent fallback
    // would hide rotation drift.
    let trusted_root = TrustedRoot::from_tuf(instance.tuf_config())
        .await
        .map_err(|e| CliError::Other(format!("TUF fetch failed: {e}")))?;

    let json = serde_json::to_vec_pretty(&trusted_root).map_err(|e| {
        CliError::Other(format!("serialising trust root to JSON failed: {e}"))
    })?;

    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    std::fs::write(&args.output, &json).map_err(|source| CliError::Io {
        path: args.output.clone(),
        source,
    })?;

    let summary = inspect_loaded(&trusted_root);
    match output.mode {
        OutputMode::Human => {
            output.success(&format!(
                "wrote trust root to {} ({})",
                args.output.display(),
                summary,
            ));
            output.status(
                "next steps: copy this file across the airgap, then set\n  \
                 [schema_forge.signing]\n  trust_root_bundle = \"/path/to/trust_root.json\"\n  \
                 in your deployment's config.toml.",
            );
        }
        OutputMode::Json => {
            output.print_json(&serde_json::json!({
                "output": args.output.display().to_string(),
                "instance": args.instance,
                "summary": summary,
            }));
        }
        OutputMode::Plain => {
            println!(
                "{}\t{}\t{}",
                args.output.display(),
                args.instance,
                summary,
            );
        }
    }

    Ok(())
}

/// Run `schemaforge trust-bundle inspect`.
pub async fn run_inspect(
    args: TrustBundleInspectArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let trusted_root = load_from_path(&args.path)?;
    let summary = inspect_loaded(&trusted_root);
    match output.mode {
        OutputMode::Human => {
            output.success(&format!("{}: {}", args.path.display(), summary));
        }
        OutputMode::Json => {
            output.print_json(&serde_json::json!({
                "path": args.path.display().to_string(),
                "summary": summary,
                "fulcio_certs": trusted_root.fulcio_certs().map(|c| c.len()).unwrap_or(0),
                "rekor_keys": trusted_root.rekor_keys().map(|k| k.len()).unwrap_or(0),
            }));
        }
        OutputMode::Plain => {
            println!("{}\t{}", args.path.display(), summary);
        }
    }
    Ok(())
}

/// Convert a loaded `TrustedRoot` into a human-readable one-liner.
/// Pulls counts of the three load-bearing collections — Fulcio CA
/// certs, Rekor signing keys, TSA cert chains — so the operator can
/// eyeball a sane-looking snapshot. Empty counts almost always mean
/// the wrong file got copied (e.g., the staging snapshot into a prod
/// deployment).
fn inspect_loaded(root: &TrustedRoot) -> String {
    let fulcio = root.fulcio_certs().map(|c| c.len()).unwrap_or(0);
    let rekor = root.rekor_keys().map(|k| k.len()).unwrap_or(0);
    let tsa = root
        .tsa_certs_with_validity()
        .map(|t| t.len())
        .unwrap_or(0);
    format!("fulcio_certs={fulcio} rekor_keys={rekor} tsa_certs={tsa}")
}

fn load_from_path(path: &Path) -> Result<TrustedRoot, CliError> {
    let bytes = std::fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    TrustedRoot::from_json(&bytes)
        .map_err(|e| CliError::from(SigningError::InvalidPolicy {
            message: format!(
                "trust root at {} failed to parse: {e}",
                path.display(),
            ),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_instance_accepts_documented_names() {
        assert!(matches!(
            parse_instance("public-good").unwrap(),
            SigstoreInstance::PublicGood
        ));
        assert!(matches!(
            parse_instance("staging").unwrap(),
            SigstoreInstance::Staging
        ));
        assert!(matches!(
            parse_instance("github").unwrap(),
            SigstoreInstance::GitHub
        ));
    }

    #[test]
    fn parse_instance_rejects_typos() {
        let err = parse_instance("publicgood").unwrap_err();
        match err {
            CliError::Other(msg) => assert!(msg.contains("unknown Sigstore instance")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn inspect_loaded_summarises_production_root() {
        // The embedded production trust root is what `from_json` in
        // sigstore-trust-root parses by default. It has multiple
        // Fulcio certs and at least one Rekor key — empty counts here
        // would indicate the parser silently dropped data.
        let root = TrustedRoot::from_json(
            sigstore_trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT,
        )
        .unwrap();
        let summary = inspect_loaded(&root);
        assert!(summary.contains("fulcio_certs="));
        assert!(summary.contains("rekor_keys="));
        // Validates that the embedded root is non-trivial — guards
        // against an upstream crate bug shipping an empty snapshot.
        assert!(
            !summary.contains("fulcio_certs=0"),
            "production root must carry at least one Fulcio cert; got {summary}"
        );
        assert!(
            !summary.contains("rekor_keys=0"),
            "production root must carry at least one Rekor key; got {summary}"
        );
    }
}
