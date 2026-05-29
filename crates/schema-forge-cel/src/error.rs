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

/// A source position: a 0-based byte offset plus 1-based line and column.
///
/// The column is counted in Unicode scalar values (chars), not bytes, so it
/// lines up with what a human sees in an editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// Byte offset into the source string.
    pub offset: usize,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number, counted in chars.
    pub column: usize,
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

/// A parse-time failure. Carries a human-readable message and, when produced by
/// the lexer/parser (#107), the source [`Position`] at which the failure occurred.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    message: String,
    position: Option<Position>,
}

impl ParseError {
    /// Construct a parse error with the given message and no position.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            position: None,
        }
    }

    /// Construct a parse error pointing at a specific source [`Position`].
    pub fn with_position(message: impl Into<String>, position: Position) -> Self {
        Self {
            message: message.into(),
            position: Some(position),
        }
    }

    /// The error message (without any positional suffix).
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The source position of the failure, if known.
    pub fn position(&self) -> Option<Position> {
        self.position
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some(pos) => write!(f, "{} at {pos}", self.message),
            None => f.write_str(&self.message),
        }
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
