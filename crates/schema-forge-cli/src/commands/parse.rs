use std::path::PathBuf;

use crate::cli::{GlobalOpts, ParseArgs};
use crate::diagnostic::render_diagnostics;
use crate::error::CliError;
use crate::output::{OutputContext, OutputMode};

/// Run the `parse` command: validate .schema files and render diagnostics.
pub async fn run(
    args: ParseArgs,
    _global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let files = discover_schema_files(&args.paths)?;

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
pub fn parse_all_schemas(
    paths: &[PathBuf],
) -> Result<Vec<schema_forge_core::types::SchemaDefinition>, CliError> {
    let files = discover_schema_files(paths)?;
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

    Ok(all_schemas)
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
}
