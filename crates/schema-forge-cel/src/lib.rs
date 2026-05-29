//! A minimal, owned CEL (Common Expression Language) evaluator for SchemaForge.
//!
//! Built from scratch over SchemaForge's `DynamicValue` domain — no third-party
//! CEL crate, no Cedar (see epic #89, decision #91). The engine is pure: no I/O,
//! no ambient authority, and guaranteed-terminating (comprehensions iterate a
//! materialized, finite range).
//!
//! It is developed test-first against the cel-spec conformance corpus, filtered
//! to the SchemaForge-relevant subset (the proto-message sections are excluded —
//! our value domain is `DynamicValue`, not protobuf messages). See the
//! `tests/conformance.rs` oracle (#90).
//!
//! ## Status
//! The parser (#107) has landed: [`parse`] turns CEL source into a typed
//! [`ast::Expr`], and [`unparse`] renders an AST back to re-parseable source. The
//! evaluator core (#108) has landed too: [`eval`] walks the AST against a
//! [`eval::Scope`] to produce a [`value::CelValue`], and [`evaluate`] wires
//! `parse` + `eval` end-to-end. The broad standard library (#109) — string,
//! numeric, and temporal built-ins — is still pending; calls to those functions
//! return a `"no such overload"` evaluation error until #109 fills them in.

pub mod ast;
pub mod error;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod value;

pub use ast::{unparse, BinaryOp, Comprehension, Expr, Literal, UnaryOp};
pub use error::{CelError, ConversionError, EvalError, ParseError, Position};
pub use eval::{eval, Scope};
pub use parser::parse;
pub use value::{CelKey, CelType, CelValue};

use std::collections::BTreeMap;

/// Variable bindings supplied to an evaluation, keyed by identifier.
pub type Bindings = BTreeMap<String, CelValue>;

/// Evaluate a CEL source expression against `bindings`.
///
/// Returns the resulting [`CelValue`], or a [`CelError`] on parse or evaluation
/// failure.
pub fn evaluate(source: &str, bindings: &Bindings) -> Result<CelValue, CelError> {
    let expr = parse(source)?;
    let scope = Scope::root(bindings);
    Ok(eval(&expr, &scope)?)
}
