//! The CEL optional-type stdlib: the `optional.*` constructors and the optional
//! value methods (`value`, `orValue`, `or`).
//!
//! `optional.of`/`optional.none`/`hasValue` are handled directly in
//! [`super::dispatch`] (they are one-liners); this module holds the functions
//! with non-trivial behaviour — zero-value detection for `ofNonZeroValue`, the
//! error case of `value()` on an absent optional, and the present/absent
//! selection of `orValue`/`or`.
//!
//! Every function is pure: it maps the already-evaluated inner [`CelValue`](s) to
//! a result with no scope, I/O, or AST recursion, so the optional semantics are
//! exhaustively unit-testable.

use crate::error::EvalError;
use crate::value::CelValue;

/// `optional.ofNonZeroValue(v)`: `optional.of(v)` unless `v` is the zero value of
/// its type, in which case `optional.none()`.
pub fn of_non_zero_value(v: &CelValue) -> CelValue {
    if is_zero_value(v) {
        CelValue::optional_none()
    } else {
        CelValue::optional_of(v.clone())
    }
}

/// Whether `v` is the CEL zero value for its type.
///
/// Mirrors cel-spec: `false`, `0`/`0u`/`0.0`, the empty string/bytes/list/map,
/// `null`, and an absent optional all count as zero. A timestamp/duration/type
/// has no zero value in this context and is treated as non-zero (so
/// `ofNonZeroValue` keeps it).
fn is_zero_value(v: &CelValue) -> bool {
    match v {
        CelValue::Null => true,
        CelValue::Bool(b) => !b,
        CelValue::Int(i) => *i == 0,
        CelValue::Uint(u) => *u == 0,
        CelValue::Double(d) => *d == 0.0,
        CelValue::String(s) => s.is_empty(),
        CelValue::Bytes(b) => b.is_empty(),
        CelValue::List(l) => l.is_empty(),
        CelValue::Map(m) => m.is_empty(),
        CelValue::Optional(o) => o.is_none(),
        CelValue::Timestamp(_) | CelValue::Duration(_) | CelValue::Type(_) => false,
    }
}

/// `opt.value()`: the inner value, or an error when the optional is absent.
pub fn value(o: &Option<Box<CelValue>>) -> Result<CelValue, EvalError> {
    match o {
        Some(v) => Ok((**v).clone()),
        None => Err(EvalError::new("optional.none() dereference")),
    }
}

/// `opt.orValue(default)`: the inner value when present, else `default`.
pub fn or_value(o: &Option<Box<CelValue>>, default: &CelValue) -> CelValue {
    match o {
        Some(v) => (**v).clone(),
        None => default.clone(),
    }
}

/// `opt.or(other)`: `opt` when present, else `other` (which must be an optional).
pub fn or(o: &Option<Box<CelValue>>, other: &CelValue) -> Result<CelValue, EvalError> {
    if o.is_some() {
        return Ok(CelValue::Optional(o.clone()));
    }
    match other {
        CelValue::Optional(_) => Ok(other.clone()),
        _ => Err(EvalError::new("no such overload")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_non_zero_value_filters_zeroes() {
        assert_eq!(
            of_non_zero_value(&CelValue::Int(0)),
            CelValue::optional_none()
        );
        assert_eq!(
            of_non_zero_value(&CelValue::Null),
            CelValue::optional_none()
        );
        assert_eq!(
            of_non_zero_value(&CelValue::String(String::new())),
            CelValue::optional_none()
        );
        assert_eq!(
            of_non_zero_value(&CelValue::Int(42)),
            CelValue::optional_of(CelValue::Int(42))
        );
    }

    #[test]
    fn value_present_and_absent() {
        assert_eq!(
            value(&Some(Box::new(CelValue::Int(7)))).unwrap(),
            CelValue::Int(7)
        );
        assert!(value(&None).is_err());
    }

    #[test]
    fn or_value_picks_inner_or_default() {
        assert_eq!(
            or_value(&Some(Box::new(CelValue::Int(7))), &CelValue::Int(0)),
            CelValue::Int(7)
        );
        assert_eq!(or_value(&None, &CelValue::Int(9)), CelValue::Int(9));
    }

    #[test]
    fn or_picks_first_present() {
        let present = Some(Box::new(CelValue::Int(1)));
        let other = CelValue::optional_of(CelValue::Int(2));
        assert_eq!(
            or(&present, &other).unwrap(),
            CelValue::optional_of(CelValue::Int(1))
        );
        // Absent receiver yields the other optional.
        assert_eq!(or(&None, &other).unwrap(), other);
        // `or` requires the argument to be an optional.
        assert_eq!(
            or(&None, &CelValue::Int(0)).unwrap_err().message(),
            "no such overload"
        );
    }
}
