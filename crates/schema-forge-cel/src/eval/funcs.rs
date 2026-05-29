//! Function-call dispatch seam plus the small set of core built-ins the #108
//! acceptance sections need.
//!
//! The evaluator eagerly evaluates a call's receiver and arguments to
//! [`CelValue`]s, then hands them here. This module dispatches on
//! `(name, arity, is_method)` to a built-in, or returns `"no such overload"` for
//! anything not yet implemented — the broad standard library (string/numeric
//! conversions, `contains`/`startsWith`/`matches`, math, timestamp/duration
//! accessors, …) lands in #109 and will extend this match.
//!
//! ## Functions implemented here (and why)
//! - `size(x)` / `x.size()` — required by the `lists` section (`size([])`,
//!   `size({...})`).
//! - `dyn(x)` — identity passthrough; required pervasively by `comparisons`
//!   (152 uses) and `lists` (22 uses) to force a dynamic-typed operand.
//! - `type(x)` — returns the runtime type value; cheap and self-contained, used
//!   by type-introspection tests across the comparison sections.

use crate::error::EvalError;
use crate::value::{CelType, CelValue};

use super::ops;

/// Dispatch a call whose receiver/arguments have already been evaluated.
///
/// `target` is `Some` for a method call (`target.name(args)`) and `None` for a
/// global call (`name(args)`).
pub fn dispatch(
    target: Option<&CelValue>,
    name: &str,
    args: &[CelValue],
) -> Result<CelValue, EvalError> {
    match (target, name, args) {
        // size: global `size(x)` or method `x.size()`.
        (None, "size", [x]) => ops::size_of(x),
        (Some(recv), "size", []) => ops::size_of(recv),
        // dyn(x): identity. Forces a dynamically-typed operand at the type level;
        // at runtime the value is unchanged.
        (None, "dyn", [x]) => Ok(x.clone()),
        // type(x): the runtime type as a `type` value.
        (None, "type", [x]) => Ok(CelValue::Type(type_name(x.cel_type()).to_string())),
        // Everything else is deferred to the #109 standard library.
        _ => Err(EvalError::new("no such overload")),
    }
}

/// The cel-spec type name for a [`CelType`].
fn type_name(t: CelType) -> &'static str {
    match t {
        CelType::Null => "null_type",
        CelType::Bool => "bool",
        CelType::Int => "int",
        CelType::Uint => "uint",
        CelType::Double => "double",
        CelType::String => "string",
        CelType::Bytes => "bytes",
        CelType::Timestamp => "google.protobuf.Timestamp",
        CelType::Duration => "google.protobuf.Duration",
        CelType::List => "list",
        CelType::Map => "map",
        CelType::Type => "type",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_global_and_method() {
        let list = CelValue::List(vec![CelValue::Int(1), CelValue::Int(2)]);
        assert_eq!(
            dispatch(None, "size", std::slice::from_ref(&list)).unwrap(),
            CelValue::Int(2)
        );
        assert_eq!(
            dispatch(Some(&list), "size", &[]).unwrap(),
            CelValue::Int(2)
        );
    }

    #[test]
    fn dyn_is_identity() {
        let v = CelValue::Int(7);
        assert_eq!(dispatch(None, "dyn", std::slice::from_ref(&v)).unwrap(), v);
    }

    #[test]
    fn type_returns_type_value() {
        assert_eq!(
            dispatch(None, "type", &[CelValue::Int(1)]).unwrap(),
            CelValue::Type("int".into())
        );
        assert_eq!(
            dispatch(None, "type", &[CelValue::String(String::new())]).unwrap(),
            CelValue::Type("string".into())
        );
    }

    #[test]
    fn unknown_function_is_no_such_overload() {
        let err = dispatch(None, "frobnicate", &[]).unwrap_err();
        assert_eq!(err.message(), "no such overload");
    }
}
