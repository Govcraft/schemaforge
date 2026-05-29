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
//! The parser (#107), evaluator core (#108), and standard library (#109) are not
//! yet implemented; [`evaluate`] currently returns [`EvalError::unimplemented`]
//! so the oracle reports honest red baselines.

pub mod error;
pub mod value;

pub use error::{CelError, ConversionError, EvalError, ParseError};
pub use value::{CelKey, CelType, CelValue};

use std::collections::BTreeMap;

/// Variable bindings supplied to an evaluation, keyed by identifier.
pub type Bindings = BTreeMap<String, CelValue>;

/// Evaluate a CEL source expression against `bindings`.
///
/// Returns the resulting [`CelValue`], or a [`CelError`] on parse or evaluation
/// failure.
pub fn evaluate(source: &str, bindings: &Bindings) -> Result<CelValue, CelError> {
    // Engine not yet built (#107–#109); reported as a red baseline by the oracle.
    let _ = (source, bindings);
    Err(EvalError::unimplemented().into())
}
