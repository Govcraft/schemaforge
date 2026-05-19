//! `schemaforge verify` — run the trust policy against a schema
//! directory and report per-file outcomes. Intended for pre-merge CI
//! gates and ad-hoc operator inspection. Does not touch the database.

use schema_forge_signing::{FileVerifyOutcome, SigningMode, VerifyPolicy, VerifyReport};

use crate::cli::{GlobalOpts, VerifyArgs};
use crate::config::{build_verify_policy, load_svc_config};
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `verify` command.
pub async fn run(
    args: VerifyArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let svc_config = load_svc_config(global)?;
    let policy = build_verify_policy(&svc_config, global)?;

    if matches!(policy.mode(), SigningMode::Off) {
        return Err(CliError::Config {
            message: "verify command requires signing.mode = \"warn\" or \"enforce\"; \
                      configure trust anchors before running `schemaforge verify`"
                .into(),
        });
    }

    let (manifest_dir, files) = crate::commands::sign::resolve_inputs_for_verify(&args.paths)?;
    let report = policy.verify_files(&manifest_dir, &files)?;
    emit_report(output, &policy, &report);

    if !report.overall_ok {
        return Err(CliError::Other(
            "one or more schemas failed verification (mode = warn). \
             Re-run with `signing.mode = \"enforce\"` to make this a hard failure."
                .into(),
        ));
    }
    Ok(())
}

fn emit_report(output: &OutputContext, policy: &VerifyPolicy, report: &VerifyReport) {
    match output.mode {
        OutputMode::Human => {
            output.success(&format!(
                "verified manifest at {} ({} trust anchors loaded, mode = {:?})",
                report.manifest_path.display(),
                policy.signer_count(),
                policy.mode()
            ));
            for f in &report.files {
                match &f.outcome {
                    FileVerifyOutcome::Verified { identity } => {
                        output.status(&format!(
                            "  ok   {} (signed by {}:{})",
                            f.path.display(),
                            identity.kind,
                            identity.name
                        ));
                    }
                    FileVerifyOutcome::Skipped => {
                        output.status(&format!("  skip {}", f.path.display()));
                    }
                    FileVerifyOutcome::Failed { reason } => {
                        output.warn(&format!("  fail {}: {reason}", f.path.display()));
                    }
                }
            }
        }
        OutputMode::Json => {
            let json = serde_json::json!({
                "mode": format!("{:?}", policy.mode()),
                "manifest_path": report.manifest_path.display().to_string(),
                "overall_ok": report.overall_ok,
                "files": report.files.iter().map(|f| serde_json::json!({
                    "path": f.path.display().to_string(),
                    "outcome": file_outcome_label(&f.outcome),
                    "detail": file_outcome_detail(&f.outcome),
                })).collect::<Vec<_>>(),
            });
            output.print_json(&json);
        }
        OutputMode::Plain => {
            for f in &report.files {
                println!(
                    "{}\t{}",
                    f.path.display(),
                    file_outcome_label(&f.outcome)
                );
            }
        }
    }
}

fn file_outcome_label(o: &FileVerifyOutcome) -> &'static str {
    match o {
        FileVerifyOutcome::Verified { .. } => "verified",
        FileVerifyOutcome::Skipped => "skipped",
        FileVerifyOutcome::Failed { .. } => "failed",
    }
}

fn file_outcome_detail(o: &FileVerifyOutcome) -> String {
    match o {
        FileVerifyOutcome::Verified { identity } => {
            format!("{}:{}", identity.kind, identity.name)
        }
        FileVerifyOutcome::Skipped => String::new(),
        FileVerifyOutcome::Failed { reason } => reason.clone(),
    }
}

