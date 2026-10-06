//! CLI adapter for shared generation primitives.
use crate::error::CliError;
use schema_forge_codegen::codegen::error::CodegenError;
pub use schema_forge_codegen::codegen::*;

impl From<CodegenError> for CliError {
    fn from(err: CodegenError) -> Self {
        match err {
            CodegenError::Io { path, source } => CliError::Io { path, source },
            other => CliError::Config {
                message: other.to_string(),
            },
        }
    }
}
