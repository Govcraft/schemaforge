use std::path::{Path, PathBuf};

use schema_forge_signing::{FileVerifyOutcome, SigningMode, VerifyPolicy};

use crate::cli::{GlobalOpts, ParseArgs};
use crate::config::{build_verify_policy, load_svc_config};
use crate::diagnostic::render_diagnostics;
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `parse` command: validate .schema files and render diagnostics.
pub async fn run(
    args: ParseArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let policy = load_verify_policy(global, output)?;
    let files = discover_schema_files(&args.paths)?;
    let manifest_dir = manifest_root_for(&args.paths, &files)?;
    verify_or_report(&policy, &manifest_dir, &files, output)?;

    let mut total_schemas = 0usize;
    let mut total_errors = 0usize;
    let mut all_file_results: Vec<serde_json::Value> = Vec::new();
    let mut had_errors = false;

    for file in &files {
        let source_text = std::fs::read_to_string(file).map_err(|e| CliError::Io {
            path: file.clone(),
            source: e,
        })?;

        let filename = file.display().to_string();

        match schema_forge_dsl::parse(&source_text) {
            Ok(schemas) => {
                let count = schemas.len();
                total_schemas += count;

                if args.print_ast {
                    let printed = schema_forge_dsl::print_all(&schemas);
                    println!("{printed}");
                }

                if output.mode == OutputMode::Json {
                    all_file_results.push(serde_json::json!({
                        "file": filename,
                        "schemas": count,
                        "errors": [],
                    }));
                } else {
                    output.status(&format!("  {filename} .... {count} schemas"));
                }
            }
            Err(errors) => {
                had_errors = true;
                let error_count = errors.len();
                total_errors += error_count;

                match output.mode {
                    OutputMode::Human => {
                        let reports = render_diagnostics(&errors, &source_text, &filename);
                        for report in &reports {
                            eprintln!("{report:?}");
                        }
                    }
                    OutputMode::Json => {
                        let error_list: Vec<serde_json::Value> = errors
                            .iter()
                            .map(|e| serde_json::json!({ "message": e.to_string() }))
                            .collect();
                        all_file_results.push(serde_json::json!({
                            "file": filename,
                            "schemas": 0,
                            "errors": error_list,
                        }));
                    }
                    OutputMode::Plain => {
                        for err in &errors {
                            eprintln!("{filename}\terror\t{err}");
                        }
                    }
                }
            }
        }
    }

    // Summary
    match output.mode {
        OutputMode::Human => {
            if had_errors {
                output.warn(&format!(
                    "{total_schemas} schemas parsed from {} files, {total_errors} errors",
                    files.len()
                ));
            } else {
                output.success(&format!(
                    "{total_schemas} schemas parsed from {} files, 0 errors",
                    files.len()
                ));
            }
        }
        OutputMode::Json => {
            let summary = serde_json::json!({
                "files": files.len(),
                "schemas": total_schemas,
                "errors": total_errors,
                "results": all_file_results,
            });
            output.print_json(&summary);
        }
        OutputMode::Plain => {
            println!("{}\t{total_schemas}\t{total_errors}", files.len());
        }
    }

    if had_errors {
        Err(CliError::Parse {
            errors: vec![], // individual errors already rendered
            source_text: String::new(),
            file: PathBuf::from("(multiple)"),
        })
    } else {
        Ok(())
    }
}

/// Discover .schema files from a list of paths.
///
/// Paths can be files (used directly) or directories (searched recursively
/// for files matching `**/*.schema`).
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
        let display_path = paths
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("schemas/"));
        return Err(CliError::NoSchemaFiles { path: display_path });
    }

    files.sort();
    files.dedup();
    Ok(files)
}

/// Parse all schema files and return the parsed definitions.
///
/// Shared helper used by `apply`, `migrate`, `export`, and `policies` commands.
/// Runs the inverse-relation pairing pass across the full batch so parent
/// `-> X[]` fields paired with a child `-> Parent` FK are marked as derived.
///
/// `verify_policy` is consulted before any source is read: under
/// `enforce` an untrusted or tampered input aborts the call before
/// the DSL ever sees the bytes. Under `warn` failures are logged
/// through `output` and parsing proceeds. Under `off` the policy is a
/// no-op and the original behaviour is preserved.
pub fn parse_all_schemas(
    paths: &[PathBuf],
    verify_policy: &VerifyPolicy,
    output: &OutputContext,
) -> Result<Vec<schema_forge_core::types::SchemaDefinition>, CliError> {
    let files = discover_schema_files(paths)?;
    let manifest_dir = manifest_root_for(paths, &files)?;
    verify_or_report(verify_policy, &manifest_dir, &files, output)?;

    let mut all_schemas = Vec::new();

    for file in &files {
        let source_text = std::fs::read_to_string(file).map_err(|e| CliError::Io {
            path: file.clone(),
            source: e,
        })?;

        match schema_forge_dsl::parse(&source_text) {
            Ok(schemas) => {
                all_schemas.extend(schemas);
            }
            Err(errors) => {
                return Err(CliError::Parse {
                    errors,
                    source_text,
                    file: file.clone(),
                });
            }
        }
    }

    schema_forge_core::inverse_relations::pair_inverse_relations(&mut all_schemas)
        .map_err(|e| CliError::Other(e.to_string()))?;

    Ok(all_schemas)
}

/// Convenience wrapper for callers that already loaded config and want
/// a one-shot "build the policy, parse the schemas" call.
pub fn parse_all_schemas_with_global(
    paths: &[PathBuf],
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<Vec<schema_forge_core::types::SchemaDefinition>, CliError> {
    let policy = load_verify_policy(global, output)?;
    parse_all_schemas(paths, &policy, output)
}

/// Decide which directory the manifest should live in. Mirrors
/// `commands::sign::resolve_inputs`: single-directory input → that
/// directory; multi-file input → their shared parent.
fn manifest_root_for(input_paths: &[PathBuf], files: &[PathBuf]) -> Result<PathBuf, CliError> {
    if input_paths.len() == 1 && input_paths[0].is_dir() {
        return Ok(input_paths[0].clone());
    }
    let mut parents: Vec<PathBuf> = Vec::new();
    for f in files {
        parents.push(f.parent().unwrap_or(f).to_path_buf());
    }
    let first = parents.first().cloned().unwrap_or_default();
    if parents.iter().all(|p| p == &first) {
        Ok(first)
    } else {
        Err(CliError::Other(
            "all schema inputs must share a single parent directory containing the manifest"
                .into(),
        ))
    }
}

/// Build the verify policy honouring `--no-verify`, the trust-policy
/// override, and the config file's `[schema_forge.signing]` section.
/// Returns [`VerifyPolicy::off`] for commands that should not verify
/// (none today), but kept as a function so callers can swap it later.
pub fn load_verify_policy(
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<VerifyPolicy, CliError> {
    let svc_config = load_svc_config(global)?;
    let policy = build_verify_policy(&svc_config, global)?;
    if matches!(policy.mode(), SigningMode::Off) {
        output.status("schema signature verification is disabled (signing.mode = off)");
    }
    Ok(policy)
}

/// Run the policy and either propagate enforce-mode failures or log
/// warn-mode failures through `output`.
fn verify_or_report(
    policy: &VerifyPolicy,
    manifest_dir: &Path,
    files: &[PathBuf],
    output: &OutputContext,
) -> Result<(), CliError> {
    let report = policy.verify_files(manifest_dir, files)?;
    if !report.overall_ok {
        // Reached only in `warn` mode — `enforce` returns Err above.
        for f in &report.files {
            if let FileVerifyOutcome::Failed { reason } = &f.outcome {
                output.warn(&format!(
                    "schema signature warning ({}): {reason}",
                    f.path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_schema_files_nonexistent_path() {
        let result = discover_schema_files(&[PathBuf::from("/nonexistent/path")]);
        assert!(result.is_err());
    }

    #[test]
    fn discover_schema_files_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let result = discover_schema_files(&[dir.path().to_path_buf()]);
        assert!(result.is_err());
    }

    #[test]
    fn discover_schema_files_finds_files() {
        let dir = tempfile::tempdir().unwrap();
        let schema_path = dir.path().join("test.schema");
        std::fs::write(&schema_path, "schema Test { name: text }").unwrap();
        let result = discover_schema_files(&[dir.path().to_path_buf()]);
        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], schema_path);
    }

    #[test]
    fn discover_schema_files_accepts_direct_file() {
        let dir = tempfile::tempdir().unwrap();
        let schema_path = dir.path().join("direct.schema");
        std::fs::write(&schema_path, "schema Direct { name: text }").unwrap();
        let result = discover_schema_files(std::slice::from_ref(&schema_path));
        assert!(result.is_ok());
        assert_eq!(result.unwrap()[0], schema_path);
    }

    #[test]
    fn discover_schema_files_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let schema_path = dir.path().join("test.schema");
        std::fs::write(&schema_path, "schema Test { name: text }").unwrap();
        let result = discover_schema_files(&[schema_path.clone(), schema_path.clone()]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn parse_all_schemas_with_off_policy_works() {
        // The off policy keeps the pre-signing behaviour: parse anything
        // in the directory without checking signatures.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.schema");
        std::fs::write(&p, "schema A { name: text }").unwrap();
        let policy = VerifyPolicy::off();
        let output = crate::output::OutputContext {
            mode: OutputMode::Plain,
            verbose: 0,
            quiet: true,
            use_color: false,
        };
        let schemas = parse_all_schemas(&[dir.path().to_path_buf()], &policy, &output).unwrap();
        assert_eq!(schemas.len(), 1);
    }

    #[test]
    fn parse_all_schemas_with_enforce_rejects_unsigned_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.schema");
        std::fs::write(&p, "schema A { name: text }").unwrap();

        let signer = schema_forge_signing::Ed25519Signer::from_seed_bytes(&[0x33; 32]);
        let cfg = schema_forge_signing::SigningConfig {
            mode: SigningMode::Enforce,
            trusted_signers: vec![schema_forge_signing::TrustedSigner::Ed25519 {
                name: "k".into(),
                public_key_b64: signer.public_key_b64_raw(),
            }],
            manifest_filename: None,
            trust_root_bundle: None,
        };
        let policy = VerifyPolicy::from_config(&cfg).unwrap();
        let output = crate::output::OutputContext {
            mode: OutputMode::Plain,
            verbose: 0,
            quiet: true,
            use_color: false,
        };
        let err = parse_all_schemas(&[dir.path().to_path_buf()], &policy, &output).unwrap_err();
        assert!(matches!(err, CliError::VerificationFailed { .. }));
    }
}
