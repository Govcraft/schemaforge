//! Function-call dispatch seam plus the small set of core built-ins the #108
//! acceptance sections need.
//!
//! The evaluator eagerly evaluates a call's receiver and arguments to
//! [`CelValue`]s, then hands them here. This module dispatches on
//! `(name, arity, is_method)` to a built-in, or returns `"no such overload"` for
//! anything not implemented.
//!
//! ## Modules
//! - this module — the `dispatch` entry point plus the self-contained core
//!   built-ins `size`, `dyn`, `type`.
//! - [`convert`] — the type-conversion functions (`int`/`uint`/`double`/`string`/
//!   `bytes`/`bool`/`timestamp`/`duration`).
//! - [`strings`] — the string predicates (`contains`/`startsWith`/`endsWith`/
//!   `matches`).
//! - [`time`] — timestamp/duration field accessors and timezone handling.
//!
//! ## Deferred standard-library functions (NON-blocking, recorded — no silent gaps)
//! The `string_ext` / `math_ext` / `encoders_ext` corpus sections are out of the
//! #109 acceptance scope and are NOT implemented here. They reach `dispatch` and
//! fall through to `"no such overload"`. Deferred names, by section:
//! - string_ext: `charAt`, `indexOf`, `lastIndexOf`, `lowerAscii`, `upperAscii`,
//!   `replace`, `split`, `substring`, `trim`, `join`, `quote`, `reverse`,
//!   `format`.
//! - math_ext (the namespaced `math.*` calls): `abs`, `ceil`, `floor`, `round`,
//!   `trunc`, `sign`, `isInf`, `isNaN`, `isFinite`, `greatest`, `least`,
//!   `bitAnd`, `bitOr`, `bitXor`, `bitNot`, `bitShiftLeft`, `bitShiftRight`.
//! - encoders_ext: `base64.encode`, `base64.decode` (`.encode()`/`.decode()`).

pub mod convert;
pub mod optional;
pub mod strings;
pub mod time;

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
        // @mapInsert(map, key, value): engine-internal accumulator step for the
        // `transformMap` macro. The `@` prefix is unspellable in surface CEL, so
        // this is reachable only from a generated comprehension step.
        (None, "@mapInsert", [map, key, value]) => map_insert(map, key, value),
        // type(x): the runtime type as a `type` value.
        (None, "type", [x]) => Ok(CelValue::Type(type_name(x.cel_type()).to_string())),

        // Optional constructors (namespaced global calls; the parser lowers
        // `optional.of(x)` etc. to these function names).
        (None, "optional.of", [x]) => Ok(CelValue::optional_of(x.clone())),
        (None, "optional.none", []) => Ok(CelValue::optional_none()),
        (None, "optional.ofNonZeroValue", [x]) => Ok(optional::of_non_zero_value(x)),
        // Optional methods.
        (Some(CelValue::Optional(o)), "hasValue", []) => Ok(CelValue::Bool(o.is_some())),
        (Some(CelValue::Optional(o)), "value", []) => optional::value(o),
        (Some(CelValue::Optional(o)), "orValue", [d]) => Ok(optional::or_value(o, d)),
        (Some(CelValue::Optional(o)), "or", [other]) => optional::or(o, other),

        // Type conversions (global one-argument functions).
        (None, "int", [x]) => convert::to_int(x),
        (None, "uint", [x]) => convert::to_uint(x),
        (None, "double", [x]) => convert::to_double(x),
        (None, "string", [x]) => convert::to_string(x),
        (None, "bytes", [x]) => convert::to_bytes(x),
        (None, "bool", [x]) => convert::to_bool(x),
        (None, "timestamp", [x]) => convert::to_timestamp(x),
        (None, "duration", [x]) => convert::to_duration(x),

        // String predicates (receiver form) and the global `matches(s, re)`.
        (Some(recv), "contains", [sub]) => strings::contains(recv, sub),
        (Some(recv), "startsWith", [pre]) => strings::starts_with(recv, pre),
        (Some(recv), "endsWith", [suf]) => strings::ends_with(recv, suf),
        (Some(recv), "matches", [re]) => strings::matches(recv, re),
        (None, "matches", [s, re]) => strings::matches(s, re),

        // Timestamp / duration field accessors (receiver form, optional tz arg).
        (Some(CelValue::Timestamp(ts)), name, []) if is_ts_accessor(name) => {
            time::timestamp_accessor(ts, name, None)
        }
        (Some(CelValue::Timestamp(ts)), name, [tz]) if is_ts_accessor(name) => {
            time::timestamp_accessor(ts, name, Some(tz))
        }
        (Some(CelValue::Duration(d)), name, []) if is_dur_accessor(name) => {
            time::duration_accessor(d, name)
        }

        // Everything else (including the deferred *_ext functions) has no overload.
        _ => Err(EvalError::new("no such overload")),
    }
}

/// Insert `(key, value)` into a copy of `map`, returning the extended map.
///
/// Backs the `transformMap` macro's accumulator step. The receiver must be a map
/// and `key` must be a legal map-key type; a duplicate key is an error, matching
/// the map-literal evaluator. In practice `transformMap` feeds unique keys (list
/// indices or distinct source-map keys), so the duplicate path is unreachable
/// in-corpus but kept for spec correctness.
fn map_insert(map: &CelValue, key: &CelValue, value: &CelValue) -> Result<CelValue, EvalError> {
    let CelValue::Map(m) = map else {
        return Err(EvalError::new("no such overload"));
    };
    let k = ops::to_key(key).ok_or_else(|| EvalError::new("no such overload"))?;
    let mut out = m.clone();
    if out.insert(k, value.clone()).is_some() {
        return Err(EvalError::new("Failed with repeated key"));
    }
    Ok(CelValue::Map(out))
}

/// Whether `name` is one of the timestamp field accessors.
fn is_ts_accessor(name: &str) -> bool {
    matches!(
        name,
        "getFullYear"
            | "getMonth"
            | "getDayOfMonth"
            | "getDate"
            | "getDayOfYear"
            | "getDayOfWeek"
            | "getHours"
            | "getMinutes"
            | "getSeconds"
            | "getMilliseconds"
    )
}

/// Whether `name` is one of the duration total-value accessors.
fn is_dur_accessor(name: &str) -> bool {
    matches!(
        name,
        "getHours" | "getMinutes" | "getSeconds" | "getMilliseconds"
    )
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
        CelType::Optional => "optional_type",
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
    fn map_insert_extends_map() {
        let map = CelValue::Map(std::collections::BTreeMap::new());
        let out = dispatch(
            None,
            "@mapInsert",
            &[map, CelValue::String("k".into()), CelValue::Int(1)],
        )
        .unwrap();
        match out {
            CelValue::Map(m) => {
                assert_eq!(
                    m.get(&crate::value::CelKey::String("k".into())),
                    Some(&CelValue::Int(1))
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    #[test]
    fn map_insert_rejects_non_map_and_bad_key() {
        // Non-map receiver.
        assert_eq!(
            dispatch(
                None,
                "@mapInsert",
                &[
                    CelValue::Int(0),
                    CelValue::String("k".into()),
                    CelValue::Int(1)
                ]
            )
            .unwrap_err()
            .message(),
            "no such overload"
        );
        // Illegal key type (a list cannot key a map).
        let map = CelValue::Map(std::collections::BTreeMap::new());
        assert_eq!(
            dispatch(
                None,
                "@mapInsert",
                &[map, CelValue::List(Vec::new()), CelValue::Int(1)]
            )
            .unwrap_err()
            .message(),
            "no such overload"
        );
    }

    #[test]
    fn unknown_function_is_no_such_overload() {
        let err = dispatch(None, "frobnicate", &[]).unwrap_err();
        assert_eq!(err.message(), "no such overload");
    }
}
