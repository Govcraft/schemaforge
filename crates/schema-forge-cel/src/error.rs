//! Error types for the CEL engine.
//!
//! Hand-written per repo convention (no `thiserror`/`anyhow`). `EvalError`'s
//! `Display` deliberately emits the CEL-spec canonical message text, because the
//! conformance oracle (#90) matches evaluation errors by message.

use std::fmt;

/// Top-level error returned by the engine.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CelError {
    /// The source expression could not be parsed.
    Parse(ParseError),
    /// Evaluation failed at runtime.
    Eval(EvalError),
    /// A value could not be converted across the CEL / `DynamicValue` boundary.
    Conversion(ConversionError),
}

impl fmt::Display for CelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::Eval(e) => write!(f, "{e}"),
            Self::Conversion(e) => write!(f, "conversion error: {e}"),
        }
    }
}

impl std::error::Error for CelError {}

impl From<ParseError> for CelError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}

impl From<EvalError> for CelError {
    fn from(e: EvalError) -> Self {
        Self::Eval(e)
    }
}

impl From<ConversionError> for CelError {
    fn from(e: ConversionError) -> Self {
        Self::Conversion(e)
    }
}

/// A parse-time failure. Carries a human-readable message; position information
/// is added when the parser lands (#107).
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    message: String,
}

impl ParseError {
    /// Construct a parse error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

/// A runtime evaluation failure. The message is the CEL-spec canonical text
/// (e.g. `"divide by zero"`, `"no_such_overload"`) so the conformance matcher
/// can compare it directly.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalError {
    message: String,
}

impl EvalError {
    /// Construct an evaluation error with the given (spec-canonical) message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Placeholder used while the engine is unimplemented (#107–#109).
    pub fn unimplemented() -> Self {
        Self::new("unimplemented: CEL engine not yet built")
    }

    /// The error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

/// A value-conversion failure across the CEL / `DynamicValue` boundary.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ConversionError {
    /// A CEL value has no `DynamicValue` representation yet (e.g. `bytes`,
    /// `duration` until field-type issues #96/#97/#98 land).
    Unsupported(String),
    /// A numeric value did not fit the target type.
    Overflow(String),
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "unsupported conversion: {what}"),
            Self::Overflow(what) => write!(f, "numeric overflow: {what}"),
        }
    }
}

impl std::error::Error for ConversionError {}
