use std::path::PathBuf;

use schema_forge_backend::BackendError;
use schema_forge_dsl::DslError;

/// Exit codes for the CLI process.
///
/// Each variant maps to a numeric exit code following standard conventions:
/// - 0: success
/// - 1: general error
/// - 2: invalid arguments / usage error
/// - 3: parse error (schema validation failure)
/// - 10+: service-specific errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ExitCode {
    Success = 0,
    GeneralError = 1,
    InvalidArguments = 2,
    ParseError = 3,
    ConnectionError = 10,
    MigrationError = 11,
}

/// Errors returned by CLI command handlers.
///
/// Each variant maps to an `ExitCode` and can produce structured
/// output in JSON mode.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Parse errors from schema-forge-dsl.
    #[error("parse errors in {file}")]
    Parse {
        errors: Vec<DslError>,
        source_text: String,
        file: PathBuf,
    },

    /// Backend connection/query errors.
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),

    /// IO errors (file not found, permission denied).
    #[error("IO error for {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Configuration errors.
    #[error("configuration error: {message}")]
    Config { message: String },

    /// User cancelled operation.
    #[error("operation cancelled")]
    Cancelled,

    /// Schema file or directory not found.
    #[error("no schema files found in {path}")]
    NoSchemaFiles { path: PathBuf },

    /// Schema not found in backend.
    #[error("schema '{name}' not found")]
    SchemaNotFound { name: String },

    /// Directory already exists (init without --force).
    #[error("directory '{path}' already exists (use --force to overwrite)")]
    DirectoryExists { path: PathBuf },

    /// Non-TTY requires --force for destructive operations.
    #[error("destructive changes require --force in non-interactive mode")]
    RequiresForce,

    /// Generic error with message.
    #[error("{0}")]
    Other(String),
}

impl CliError {
    /// Maps this error to the appropriate exit code.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Parse { .. } => ExitCode::ParseError,
            Self::Backend(BackendError::ConnectionError { .. }) => ExitCode::ConnectionError,
            Self::Backend(BackendError::MigrationFailed { .. }) => ExitCode::MigrationError,
            Self::Backend(_) => ExitCode::GeneralError,
            Self::Config { .. } | Self::NoSchemaFiles { .. } => ExitCode::InvalidArguments,
            Self::Io { .. }
            | Self::Cancelled
            | Self::SchemaNotFound { .. }
            | Self::DirectoryExists { .. }
            | Self::RequiresForce
            | Self::Other(_) => ExitCode::GeneralError,
        }
    }

    /// Serializes this error as a JSON value for `--format json` output.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Parse { errors, file, .. } => {
                let error_list: Vec<serde_json::Value> = errors
                    .iter()
                    .map(|e| serde_json::json!({ "message": e.to_string() }))
                    .collect();
                serde_json::json!({
                    "error": "parse_error",
                    "file": file.display().to_string(),
                    "errors": error_list,
                })
            }
            Self::Backend(e) => serde_json::json!({
                "error": "backend_error",
                "message": e.to_string(),
            }),
            Self::Io { path, source } => serde_json::json!({
                "error": "io_error",
                "path": path.display().to_string(),
                "message": source.to_string(),
            }),
            Self::Config { message } => serde_json::json!({
                "error": "config_error",
                "message": message,
            }),
            other => serde_json::json!({
                "error": "error",
                "message": other.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_dsl::Span;

    #[test]
    fn parse_error_exit_code() {
        let err = CliError::Parse {
            errors: vec![DslError::InvalidToken {
                span: Span::new(0, 1),
            }],
            source_text: "x".into(),
            file: PathBuf::from("test.schema"),
        };
        assert_eq!(err.exit_code(), ExitCode::ParseError);
    }

    #[test]
    fn backend_connection_error_exit_code() {
        let err = CliError::Backend(BackendError::ConnectionError {
            message: "refused".into(),
        });
        assert_eq!(err.exit_code(), ExitCode::ConnectionError);
    }

    #[test]
    fn backend_migration_error_exit_code() {
        let err = CliError::Backend(BackendError::MigrationFailed {
            step: "test".into(),
            reason: "fail".into(),
        });
        assert_eq!(err.exit_code(), ExitCode::MigrationError);
    }

    #[test]
    fn config_error_exit_code() {
        let err = CliError::Config {
            message: "bad config".into(),
        };
        assert_eq!(err.exit_code(), ExitCode::InvalidArguments);
    }

    #[test]
    fn no_schema_files_exit_code() {
        let err = CliError::NoSchemaFiles {
            path: PathBuf::from("schemas/"),
        };
        assert_eq!(err.exit_code(), ExitCode::InvalidArguments);
    }

    #[test]
    fn cancelled_exit_code() {
        let err = CliError::Cancelled;
        assert_eq!(err.exit_code(), ExitCode::GeneralError);
    }

    #[test]
    fn other_exit_code() {
        let err = CliError::Other("something".into());
        assert_eq!(err.exit_code(), ExitCode::GeneralError);
    }

    #[test]
    fn display_parse_error() {
        let err = CliError::Parse {
            errors: vec![],
            source_text: String::new(),
            file: PathBuf::from("test.schema"),
        };
        assert!(err.to_string().contains("test.schema"));
    }

    #[test]
    fn display_backend_error() {
        let err = CliError::Backend(BackendError::SchemaNotFound {
            schema: "Contact".into(),
        });
        assert!(err.to_string().contains("backend error"));
    }

    #[test]
    fn display_directory_exists() {
        let err = CliError::DirectoryExists {
            path: PathBuf::from("my-project"),
        };
        assert!(err.to_string().contains("my-project"));
        assert!(err.to_string().contains("--force"));
    }

    #[test]
    fn to_json_parse_error() {
        let err = CliError::Parse {
            errors: vec![DslError::InvalidToken {
                span: Span::new(0, 1),
            }],
            source_text: "x".into(),
            file: PathBuf::from("test.schema"),
        };
        let json = err.to_json();
        assert_eq!(json["error"], "parse_error");
        assert_eq!(json["file"], "test.schema");
        assert!(json["errors"].is_array());
    }

    #[test]
    fn to_json_backend_error() {
        let err = CliError::Backend(BackendError::ConnectionError {
            message: "refused".into(),
        });
        let json = err.to_json();
        assert_eq!(json["error"], "backend_error");
        assert!(json["message"].as_str().unwrap().contains("refused"));
    }

    #[test]
    fn to_json_io_error() {
        let err = CliError::Io {
            path: PathBuf::from("/tmp/file"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        let json = err.to_json();
        assert_eq!(json["error"], "io_error");
        assert_eq!(json["path"], "/tmp/file");
    }

    #[test]
    fn to_json_config_error() {
        let err = CliError::Config {
            message: "bad value".into(),
        };
        let json = err.to_json();
        assert_eq!(json["error"], "config_error");
    }

    #[test]
    fn to_json_other_error() {
        let err = CliError::Other("unexpected".into());
        let json = err.to_json();
        assert_eq!(json["error"], "error");
        assert!(json["message"].as_str().unwrap().contains("unexpected"));
    }

    #[test]
    fn exit_code_values() {
        assert_eq!(ExitCode::Success as i32, 0);
        assert_eq!(ExitCode::GeneralError as i32, 1);
        assert_eq!(ExitCode::InvalidArguments as i32, 2);
        assert_eq!(ExitCode::ParseError as i32, 3);
        assert_eq!(ExitCode::ConnectionError as i32, 10);
        assert_eq!(ExitCode::MigrationError as i32, 11);
    }
}
