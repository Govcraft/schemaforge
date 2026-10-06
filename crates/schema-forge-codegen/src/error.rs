//! Errors raised while building a rendered site plan.
use std::path::PathBuf;
/// Site configuration, template rendering, or asset-reading failure.
#[derive(Debug)]
pub enum GenerationError {
    /// Invalid schema projection or rendering configuration.
    Config { message: String },
    /// Asset read failed at the given path.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}
impl std::fmt::Display for GenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config { message } => f.write_str(message),
            Self::Io { path, source } => write!(f, "io error for {}: {source}", path.display()),
        }
    }
}
impl std::error::Error for GenerationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Config { .. } => None,
        }
    }
}
