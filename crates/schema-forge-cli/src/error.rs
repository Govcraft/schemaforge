use std::path::PathBuf;

use schema_forge_backend::BackendError;
use schema_forge_dsl::DslError;
use schema_forge_signing::{SigningError, VerifyError};

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
    ServerError = 12,
    /// Schema signature verification failed. Distinguished from
    /// `GeneralError` so CI gates and audit pipelines can branch on
    /// "untrusted schema content" specifically.
    VerificationFailed = 13,
    /// Authentication rejected by the server (HTTP 401). Distinct from a
    /// transport `ConnectionError` so scripts can tell "token is missing or
    /// expired" apart from "the server is unreachable".
    AuthFailed = 14,
    /// Authorization denied by the server (HTTP 403). The caller
    /// authenticated successfully but lacks permission for the action.
    Forbidden = 15,
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

    /// HTTP server errors.
    #[error("server error: {message}")]
    Server { message: String },

    /// A non-success response from the entity REST API. Carries the HTTP
    /// status and the server's error envelope (`{ "error": kind, "message":
    /// ... }`). The `message` is already human-formatted by
    /// [`crate::http::classify_http_error`] — including grouped validation
    /// `details` and an actionable tip — so `Display` can render it
    /// verbatim. `status` and `kind` drive the exit code and JSON output.
    #[error("{message}")]
    Http {
        status: u16,
        kind: String,
        message: String,
    },

    /// The HTTP request never reached a server response: connection
    /// refused, DNS failure, TLS handshake error, or timeout. Distinct from
    /// [`CliError::Http`] (the server answered) and from
    /// [`BackendError::ConnectionError`] (a direct database connection).
    #[error("connection error: {message}")]
    Connection { message: String },

    /// Schema-signature verification rejected the input. Wraps the
    /// underlying [`VerifyError`] so the diagnostic layer can render
    /// the specific failure (missing signature, hash mismatch,
    /// untrusted signer, …).
    #[error("schema signature verification failed: {source}")]
    VerificationFailed {
        #[source]
        source: VerifyError,
    },

    /// Problem with the signing trust policy or with producing a
    /// signature (key parse failure, malformed manifest, IO during
    /// signing). Distinct from `VerificationFailed` which represents
    /// a negative *outcome* — this one represents broken inputs.
    #[error("signing infrastructure error: {source}")]
    Signing {
        #[source]
        source: SigningError,
    },

    /// Generic error with message.
    #[error("{0}")]
    Other(String),
}

impl From<VerifyError> for CliError {
    fn from(source: VerifyError) -> Self {
        Self::VerificationFailed { source }
    }
}

impl From<SigningError> for CliError {
    fn from(source: SigningError) -> Self {
        Self::Signing { source }
    }
}

impl CliError {
    /// Maps this error to the appropriate exit code.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Parse { .. } => ExitCode::ParseError,
            Self::Backend(BackendError::ConnectionError { .. }) => ExitCode::ConnectionError,
            Self::Backend(BackendError::MigrationFailed { .. }) => ExitCode::MigrationError,
            Self::Backend(_) => ExitCode::GeneralError,
            Self::Config { .. } | Self::NoSchemaFiles { .. } | Self::Signing { .. } => {
                ExitCode::InvalidArguments
            }
            Self::Server { .. } => ExitCode::ServerError,
            Self::Http { status, .. } => match status {
                401 => ExitCode::AuthFailed,
                403 => ExitCode::Forbidden,
                422 => ExitCode::InvalidArguments,
                500..=599 => ExitCode::ServerError,
                _ => ExitCode::GeneralError,
            },
            Self::Connection { .. } => ExitCode::ConnectionError,
            Self::VerificationFailed { .. } => ExitCode::VerificationFailed,
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
            Self::Server { message } => serde_json::json!({
                "error": "server_error",
                "message": message,
            }),
            Self::Http {
                status,
                kind,
                message,
            } => serde_json::json!({
                "error": kind,
                "message": message,
                "status": status,
            }),
            Self::Connection { message } => serde_json::json!({
                "error": "connection_error",
                "message": message,
            }),
            Self::VerificationFailed { source } => serde_json::json!({
                "error": "verification_failed",
                "message": source.to_string(),
            }),
            Self::Signing { source } => serde_json::json!({
                "error": "signing_error",
                "message": source.to_string(),
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
    fn server_error_exit_code() {
        let err = CliError::Server {
            message: "bind failed".into(),
        };
        assert_eq!(err.exit_code(), ExitCode::ServerError);
    }

    #[test]
    fn display_server_error() {
        let err = CliError::Server {
            message: "port in use".into(),
        };
        assert!(err.to_string().contains("server error"));
        assert!(err.to_string().contains("port in use"));
    }

    #[test]
    fn to_json_server_error() {
        let err = CliError::Server {
            message: "shutdown".into(),
        };
        let json = err.to_json();
        assert_eq!(json["error"], "server_error");
        assert_eq!(json["message"], "shutdown");
    }

    #[test]
    fn exit_code_values() {
        assert_eq!(ExitCode::Success as i32, 0);
        assert_eq!(ExitCode::GeneralError as i32, 1);
        assert_eq!(ExitCode::InvalidArguments as i32, 2);
        assert_eq!(ExitCode::ParseError as i32, 3);
        assert_eq!(ExitCode::ConnectionError as i32, 10);
        assert_eq!(ExitCode::MigrationError as i32, 11);
        assert_eq!(ExitCode::ServerError as i32, 12);
        assert_eq!(ExitCode::VerificationFailed as i32, 13);
        assert_eq!(ExitCode::AuthFailed as i32, 14);
        assert_eq!(ExitCode::Forbidden as i32, 15);
    }

    fn http(status: u16) -> CliError {
        CliError::Http {
            status,
            kind: "x".into(),
            message: "boom".into(),
        }
    }

    #[test]
    fn http_status_maps_to_exit_code() {
        assert_eq!(http(401).exit_code(), ExitCode::AuthFailed);
        assert_eq!(http(403).exit_code(), ExitCode::Forbidden);
        assert_eq!(http(422).exit_code(), ExitCode::InvalidArguments);
        assert_eq!(http(404).exit_code(), ExitCode::GeneralError);
        assert_eq!(http(409).exit_code(), ExitCode::GeneralError);
        assert_eq!(http(500).exit_code(), ExitCode::ServerError);
        assert_eq!(http(503).exit_code(), ExitCode::ServerError);
    }

    #[test]
    fn connection_error_exit_code() {
        let err = CliError::Connection {
            message: "refused".into(),
        };
        assert_eq!(err.exit_code(), ExitCode::ConnectionError);
    }

    #[test]
    fn to_json_http_error_uses_server_kind() {
        let err = CliError::Http {
            status: 422,
            kind: "validation_failed".into(),
            message: "2 fields invalid".into(),
        };
        let json = err.to_json();
        assert_eq!(json["error"], "validation_failed");
        assert_eq!(json["status"], 422);
        assert_eq!(json["message"], "2 fields invalid");
    }

    #[test]
    fn to_json_connection_error() {
        let err = CliError::Connection {
            message: "dns failure".into(),
        };
        let json = err.to_json();
        assert_eq!(json["error"], "connection_error");
        assert!(json["message"].as_str().unwrap().contains("dns"));
    }

    #[test]
    fn verification_failed_exit_code() {
        let err = CliError::VerificationFailed {
            source: VerifyError::MissingManifest {
                path: PathBuf::from("schemas/schemas.manifest.toml"),
            },
        };
        assert_eq!(err.exit_code(), ExitCode::VerificationFailed);
    }

    #[test]
    fn signing_error_exit_code() {
        let err = CliError::Signing {
            source: SigningError::InvalidPolicy {
                message: "no signers".into(),
            },
        };
        assert_eq!(err.exit_code(), ExitCode::InvalidArguments);
    }

    #[test]
    fn to_json_verification_failed() {
        let err = CliError::VerificationFailed {
            source: VerifyError::MissingManifest {
                path: PathBuf::from("schemas/schemas.manifest.toml"),
            },
        };
        let json = err.to_json();
        assert_eq!(json["error"], "verification_failed");
        assert!(json["message"].as_str().unwrap().contains("no manifest"));
    }
}
