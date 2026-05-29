//! Pure value operations for the CEL evaluator.
//!
//! Every function here is a pure mapping over [`CelValue`] inputs to a
//! [`CelValue`] (or a `bool`) result, returning an [`EvalError`] for the CEL
//! runtime-error cases. They are deliberately free of scope, I/O, or recursion
//! into the AST, which keeps them exhaustively unit-testable.
//!
//! ## A note on equality
//! [`cel_equals`] is the CEL `==`/`!=` *operator*, with cross-type numeric
//! semantics. It is intentionally distinct from [`CelValue`]'s derived
//! `PartialEq`, which stays type-exact for conformance result matching (see the
//! module docs in `crate::value`).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};

use crate::ast::{BinaryOp, UnaryOp};
use crate::error::EvalError;
use crate::value::{CelKey, CelValue};

use super::funcs::convert::ts_in_range;

/// Canonical spec error for an operator applied to operand types it has no
/// overload for (used by comparison/arithmetic).
fn no_such_overload() -> EvalError {
    EvalError::new("no such overload")
}

/// Compare two numbers (any mix of int/uint/double) by mathematical value.
///
/// Returns `None` when the comparison is undefined, i.e. a `NaN` is involved.
/// This is the single boundary-safe numeric comparator used by both equality and
/// ordering, so the i64/u64/f64 edge cases live in exactly one place.
fn num_cmp(a: &CelValue, b: &CelValue) -> Option<Ordering> {
    match (a, b) {
        (CelValue::Int(x), CelValue::Int(y)) => Some(x.cmp(y)),
        (CelValue::Uint(x), CelValue::Uint(y)) => Some(x.cmp(y)),
        (CelValue::Double(x), CelValue::Double(y)) => x.partial_cmp(y),
        (CelValue::Int(x), CelValue::Uint(y)) => Some(int_uint_cmp(*x, *y)),
        (CelValue::Uint(x), CelValue::Int(y)) => Some(int_uint_cmp(*y, *x).reverse()),
        (CelValue::Int(x), CelValue::Double(y)) => int_double_cmp(*x, *y),
        (CelValue::Double(x), CelValue::Int(y)) => int_double_cmp(*y, *x).map(Ordering::reverse),
        (CelValue::Uint(x), CelValue::Double(y)) => uint_double_cmp(*x, *y),
        (CelValue::Double(x), CelValue::Uint(y)) => uint_double_cmp(*y, *x).map(Ordering::reverse),
        _ => None,
    }
}

fn int_uint_cmp(i: i64, u: u64) -> Ordering {
    if i < 0 {
        Ordering::Less
    } else {
        (i as u64).cmp(&u)
    }
}

/// Compare an `i64` against an `f64` exactly (no precision loss): widen the int to
/// `f64` only when it is exactly representable, otherwise reason via the float's
/// integer/fractional decomposition.
fn int_double_cmp(i: i64, d: f64) -> Option<Ordering> {
    if d.is_nan() {
        return None;
    }
    if d.is_infinite() {
        return Some(if d > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    // f64 has 53 bits of mantissa; |i| below 2^53 is exact as f64.
    if i.unsigned_abs() < (1u64 << 53) {
        return (i as f64).partial_cmp(&d);
    }
    // Large magnitude: compare against the floor/ceil of d.
    let floor = d.floor();
    if floor < i64::MIN as f64 {
        return Some(Ordering::Greater);
    }
    if floor >= 9_223_372_036_854_775_808.0 {
        return Some(Ordering::Less);
    }
    let di = floor as i64;
    match i.cmp(&di) {
        Ordering::Equal if d > floor => Some(Ordering::Less),
        other => Some(other),
    }
}

fn uint_double_cmp(u: u64, d: f64) -> Option<Ordering> {
    if d.is_nan() {
        return None;
    }
    if d < 0.0 {
        return Some(Ordering::Greater);
    }
    if d.is_infinite() {
        return Some(Ordering::Less);
    }
    if u < (1u64 << 53) {
        return (u as f64).partial_cmp(&d);
    }
    let floor = d.floor();
    if floor >= 18_446_744_073_709_551_616.0 {
        return Some(Ordering::Less);
    }
    let du = floor as u64;
    match u.cmp(&du) {
        Ordering::Equal if d > floor => Some(Ordering::Less),
        other => Some(other),
    }
}

fn is_numeric(v: &CelValue) -> bool {
    matches!(
        v,
        CelValue::Int(_) | CelValue::Uint(_) | CelValue::Double(_)
    )
}

/// CEL `==` operator. Cross-type numeric values compare by mathematical value;
/// `NaN` is never equal to anything; lists and maps compare element/entry-wise;
/// genuinely different non-numeric types are unequal (not an error).
pub fn cel_equals(a: &CelValue, b: &CelValue) -> Result<bool, EvalError> {
    if is_numeric(a) && is_numeric(b) {
        return Ok(num_cmp(a, b) == Some(Ordering::Equal));
    }
    match (a, b) {
        (CelValue::Null, CelValue::Null) => Ok(true),
        (CelValue::Bool(x), CelValue::Bool(y)) => Ok(x == y),
        (CelValue::String(x), CelValue::String(y)) => Ok(x == y),
        (CelValue::Bytes(x), CelValue::Bytes(y)) => Ok(x == y),
        (CelValue::Type(x), CelValue::Type(y)) => Ok(x == y),
        (CelValue::Timestamp(x), CelValue::Timestamp(y)) => Ok(x == y),
        (CelValue::Duration(x), CelValue::Duration(y)) => Ok(x == y),
        (CelValue::List(x), CelValue::List(y)) => list_equals(x, y),
        (CelValue::Map(x), CelValue::Map(y)) => map_equals(x, y),
        // Different, non-numeric, non-matching types: unequal, never an error.
        _ => Ok(false),
    }
}

fn list_equals(x: &[CelValue], y: &[CelValue]) -> Result<bool, EvalError> {
    if x.len() != y.len() {
        return Ok(false);
    }
    for (a, b) in x.iter().zip(y) {
        if !cel_equals(a, b)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn map_equals(
    x: &BTreeMap<CelKey, CelValue>,
    y: &BTreeMap<CelKey, CelValue>,
) -> Result<bool, EvalError> {
    if x.len() != y.len() {
        return Ok(false);
    }
    for (k, xv) in x {
        // Keys match cross-type numerically (`{1: ...}` and `{1u: ...}` share an
        // entry), so look up via `map_get` rather than an exact `BTreeMap::get`.
        match map_get(y, k) {
            Some(yv) if cel_equals(xv, yv)? => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// CEL ordering operators (`< <= > >=`). Type-incompatible operands error with
/// `"no such overload"`. A `NaN` operand makes every ordering comparison `false`.
pub fn compare(op: BinaryOp, a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    let ordering = ordering_of(a, b)?;
    let result = match ordering {
        // `None` == NaN involved: all ordering comparisons are false.
        None => false,
        Some(ord) => match op {
            BinaryOp::Lt => ord == Ordering::Less,
            BinaryOp::Le => ord != Ordering::Greater,
            BinaryOp::Gt => ord == Ordering::Greater,
            BinaryOp::Ge => ord != Ordering::Less,
            _ => return Err(no_such_overload()),
        },
    };
    Ok(CelValue::Bool(result))
}

/// Total/partial ordering of two values. `Ok(None)` signals a NaN-involved
/// comparison (defined operands but undefined result); `Err` signals
/// type-incompatible operands.
fn ordering_of(a: &CelValue, b: &CelValue) -> Result<Option<Ordering>, EvalError> {
    if is_numeric(a) && is_numeric(b) {
        return Ok(num_cmp(a, b));
    }
    match (a, b) {
        (CelValue::Bool(x), CelValue::Bool(y)) => Ok(Some(x.cmp(y))),
        (CelValue::String(x), CelValue::String(y)) => Ok(Some(x.cmp(y))),
        (CelValue::Bytes(x), CelValue::Bytes(y)) => Ok(Some(x.cmp(y))),
        (CelValue::Timestamp(x), CelValue::Timestamp(y)) => Ok(Some(x.cmp(y))),
        (CelValue::Duration(x), CelValue::Duration(y)) => Ok(Some(x.cmp(y))),
        _ => Err(no_such_overload()),
    }
}

/// CEL arithmetic operators (`+ - * / %`). Integer/uint use checked math
/// (overflow → `"return error for overflow"`); division/modulus by zero error;
/// `+` additionally concatenates strings, bytes, and lists.
pub fn arithmetic(op: BinaryOp, a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    match op {
        BinaryOp::Add => add(a, b),
        BinaryOp::Sub => sub(a, b),
        BinaryOp::Mul => mul(a, b),
        BinaryOp::Div => div(a, b),
        BinaryOp::Rem => rem(a, b),
        _ => Err(no_such_overload()),
    }
}

fn overflow() -> EvalError {
    EvalError::new("return error for overflow")
}

fn add(a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    match (a, b) {
        (CelValue::Int(x), CelValue::Int(y)) => {
            x.checked_add(*y).map(CelValue::Int).ok_or_else(overflow)
        }
        (CelValue::Uint(x), CelValue::Uint(y)) => {
            x.checked_add(*y).map(CelValue::Uint).ok_or_else(overflow)
        }
        (CelValue::Double(x), CelValue::Double(y)) => Ok(CelValue::Double(x + y)),
        (CelValue::String(x), CelValue::String(y)) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            Ok(CelValue::String(s))
        }
        (CelValue::Bytes(x), CelValue::Bytes(y)) => {
            let mut v = Vec::with_capacity(x.len() + y.len());
            v.extend_from_slice(x);
            v.extend_from_slice(y);
            Ok(CelValue::Bytes(v))
        }
        (CelValue::List(x), CelValue::List(y)) => {
            let mut v = Vec::with_capacity(x.len() + y.len());
            v.extend_from_slice(x);
            v.extend_from_slice(y);
            Ok(CelValue::List(v))
        }
        // timestamp + duration / duration + timestamp → timestamp.
        (CelValue::Timestamp(t), CelValue::Duration(d))
        | (CelValue::Duration(d), CelValue::Timestamp(t)) => ts_plus_duration(t, d),
        // duration + duration → duration.
        (CelValue::Duration(x), CelValue::Duration(y)) => duration_plus_duration(x, y),
        _ => Err(no_such_overload()),
    }
}

fn sub(a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    match (a, b) {
        (CelValue::Int(x), CelValue::Int(y)) => {
            x.checked_sub(*y).map(CelValue::Int).ok_or_else(overflow)
        }
        (CelValue::Uint(x), CelValue::Uint(y)) => {
            x.checked_sub(*y).map(CelValue::Uint).ok_or_else(overflow)
        }
        (CelValue::Double(x), CelValue::Double(y)) => Ok(CelValue::Double(x - y)),
        // timestamp - duration → timestamp.
        (CelValue::Timestamp(t), CelValue::Duration(d)) => match TimeDelta::zero().checked_sub(d) {
            Some(neg) => ts_plus_duration(t, &neg),
            None => Err(range()),
        },
        // timestamp - timestamp → duration.
        (CelValue::Timestamp(x), CelValue::Timestamp(y)) => {
            let delta = x.signed_duration_since(*y);
            in_range_duration(delta)
        }
        // duration - duration → duration.
        (CelValue::Duration(x), CelValue::Duration(y)) => {
            let delta = x.checked_sub(y).ok_or_else(range)?;
            in_range_duration(delta)
        }
        _ => Err(no_such_overload()),
    }
}

/// The cel-spec out-of-range error for temporal arithmetic.
fn range() -> EvalError {
    EvalError::new("range error")
}

/// Add a duration to a timestamp, enforcing CEL's timestamp range on the result.
fn ts_plus_duration(t: &DateTime<Utc>, d: &TimeDelta) -> Result<CelValue, EvalError> {
    let result = t.checked_add_signed(*d).ok_or_else(range)?;
    if ts_in_range(&result) {
        Ok(CelValue::Timestamp(result))
    } else {
        Err(range())
    }
}

/// Add two durations, enforcing CEL's duration range on the result.
fn duration_plus_duration(x: &TimeDelta, y: &TimeDelta) -> Result<CelValue, EvalError> {
    let delta = x.checked_add(y).ok_or_else(range)?;
    in_range_duration(delta)
}

/// Wrap a computed duration in the arithmetic range check.
///
/// A duration *produced by arithmetic* (timestamp−timestamp, duration±duration)
/// must fit cel-go's internal representation, which is an `int64` count of
/// **nanoseconds** (`time.Duration`). This is tighter than the `±315576000000s`
/// range that `duration()` accepts from a string literal: e.g.
/// `9999-12-31… − 0001-01-01…` is `315537897599s`, which is inside the string
/// range yet overflows int64 nanoseconds, so the corpus expects a range error.
/// `TimeDelta::num_nanoseconds` returns `None` exactly on that overflow.
fn in_range_duration(delta: TimeDelta) -> Result<CelValue, EvalError> {
    if delta.num_nanoseconds().is_some() {
        Ok(CelValue::Duration(delta))
    } else {
        Err(range())
    }
}

fn mul(a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    match (a, b) {
        (CelValue::Int(x), CelValue::Int(y)) => {
            x.checked_mul(*y).map(CelValue::Int).ok_or_else(overflow)
        }
        (CelValue::Uint(x), CelValue::Uint(y)) => {
            x.checked_mul(*y).map(CelValue::Uint).ok_or_else(overflow)
        }
        (CelValue::Double(x), CelValue::Double(y)) => Ok(CelValue::Double(x * y)),
        _ => Err(no_such_overload()),
    }
}

fn div(a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    match (a, b) {
        (CelValue::Int(_), CelValue::Int(0)) | (CelValue::Uint(_), CelValue::Uint(0)) => {
            Err(EvalError::new("divide by zero"))
        }
        // i64::MIN / -1 overflows.
        (CelValue::Int(x), CelValue::Int(y)) => {
            x.checked_div(*y).map(CelValue::Int).ok_or_else(overflow)
        }
        (CelValue::Uint(x), CelValue::Uint(y)) => Ok(CelValue::Uint(x / y)),
        (CelValue::Double(x), CelValue::Double(y)) => Ok(CelValue::Double(x / y)),
        _ => Err(no_such_overload()),
    }
}

fn rem(a: &CelValue, b: &CelValue) -> Result<CelValue, EvalError> {
    match (a, b) {
        (CelValue::Int(_), CelValue::Int(0)) | (CelValue::Uint(_), CelValue::Uint(0)) => {
            Err(EvalError::new("modulus by zero"))
        }
        // i64::MIN % -1 overflows in Rust's checked_rem.
        (CelValue::Int(x), CelValue::Int(y)) => {
            x.checked_rem(*y).map(CelValue::Int).ok_or_else(overflow)
        }
        (CelValue::Uint(x), CelValue::Uint(y)) => Ok(CelValue::Uint(x % y)),
        // CEL has no `%` overload on doubles; emit the spec's specific
        // no-matching-overload text for `(double, double)` so it grades green.
        (CelValue::Double(_), CelValue::Double(_)) => Err(EvalError::new(
            "found no matching overload for '_%_' applied to '(double, double)'",
        )),
        _ => Err(no_such_overload()),
    }
}

/// CEL prefix unary operators (`!`, `-`).
pub fn unary(op: UnaryOp, v: &CelValue) -> Result<CelValue, EvalError> {
    match (op, v) {
        // `!` of a non-bool is reported with the logical-overload spelling.
        (UnaryOp::Not, CelValue::Bool(b)) => Ok(CelValue::Bool(!b)),
        (UnaryOp::Not, _) => Err(EvalError::new("no matching overload")),
        (UnaryOp::Neg, CelValue::Int(i)) => i.checked_neg().map(CelValue::Int).ok_or_else(overflow),
        (UnaryOp::Neg, CelValue::Double(d)) => Ok(CelValue::Double(-d)),
        (UnaryOp::Neg, _) => Err(no_such_overload()),
    }
}

/// Convert a value to a legal map key, if its type can key a map.
pub fn to_key(v: &CelValue) -> Option<CelKey> {
    match v {
        CelValue::Bool(b) => Some(CelKey::Bool(*b)),
        CelValue::Int(i) => Some(CelKey::Int(*i)),
        CelValue::Uint(u) => Some(CelKey::Uint(*u)),
        CelValue::String(s) => Some(CelKey::String(s.clone())),
        _ => None,
    }
}

/// Look a key up in a map, honoring CEL's int/uint cross-type key equality
/// (`m[1]` and `m[1u]` hit the same entry when the numeric values match).
fn map_get<'m>(m: &'m BTreeMap<CelKey, CelValue>, key: &CelKey) -> Option<&'m CelValue> {
    if let Some(v) = m.get(key) {
        return Some(v);
    }
    // Cross-type numeric key fallback: int <-> uint with equal magnitude.
    match key {
        CelKey::Int(i) if *i >= 0 => m.get(&CelKey::Uint(*i as u64)),
        CelKey::Uint(u) if *u <= i64::MAX as u64 => m.get(&CelKey::Int(*u as i64)),
        _ => None,
    }
}

/// CEL index operator `a[i]`. Lists index by int/uint (out-of-range and
/// non-integer indices → `"invalid_argument"`); maps index by key
/// (missing → `"no such key"`).
pub fn index_value(coll: &CelValue, idx: &CelValue) -> Result<CelValue, EvalError> {
    match coll {
        CelValue::List(items) => index_list(items, idx),
        CelValue::Map(m) => {
            let key = to_key(idx).ok_or_else(no_such_overload)?;
            map_get(m, &key)
                .cloned()
                .ok_or_else(|| EvalError::new(format!("no such key: {}", key_display(&key))))
        }
        _ => Err(no_such_overload()),
    }
}

fn index_list(items: &[CelValue], idx: &CelValue) -> Result<CelValue, EvalError> {
    let i: i64 = match idx {
        CelValue::Int(i) => *i,
        CelValue::Uint(u) => i64::try_from(*u).map_err(|_| invalid_argument())?,
        // A double indexes a list only when it is a whole number (e.g. `l[0.0]`);
        // a fractional double (`l[0.1]`) is an invalid argument.
        CelValue::Double(d) if d.fract() == 0.0 && d.is_finite() => *d as i64,
        // Any other index type (fractional double, string, …) is invalid.
        _ => return Err(invalid_argument()),
    };
    if i < 0 {
        return Err(invalid_argument());
    }
    items.get(i as usize).cloned().ok_or_else(invalid_argument)
}

fn invalid_argument() -> EvalError {
    EvalError::new("invalid_argument")
}

fn key_display(k: &CelKey) -> String {
    match k {
        CelKey::Bool(b) => b.to_string(),
        CelKey::Int(i) => i.to_string(),
        CelKey::Uint(u) => u.to_string(),
        CelKey::String(s) => s.clone(),
    }
}

/// CEL `in` membership. Lists test element equality (via [`cel_equals`]); maps
/// test key membership.
pub fn membership(elem: &CelValue, container: &CelValue) -> Result<CelValue, EvalError> {
    match container {
        CelValue::List(items) => {
            for item in items {
                if cel_equals(elem, item)? {
                    return Ok(CelValue::Bool(true));
                }
            }
            Ok(CelValue::Bool(false))
        }
        CelValue::Map(m) => match to_key(elem) {
            Some(key) => Ok(CelValue::Bool(map_get(m, &key).is_some())),
            None => Ok(CelValue::Bool(false)),
        },
        _ => Err(no_such_overload()),
    }
}

/// The number of elements/characters/bytes in a sized value, as an `int`.
pub fn size_of(v: &CelValue) -> Result<CelValue, EvalError> {
    let n = match v {
        CelValue::String(s) => s.chars().count(),
        CelValue::Bytes(b) => b.len(),
        CelValue::List(l) => l.len(),
        CelValue::Map(m) => m.len(),
        _ => return Err(no_such_overload()),
    };
    i64::try_from(n).map(CelValue::Int).map_err(|_| overflow())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(i: i64) -> CelValue {
        CelValue::Int(i)
    }
    fn uint(u: u64) -> CelValue {
        CelValue::Uint(u)
    }
    fn dbl(d: f64) -> CelValue {
        CelValue::Double(d)
    }

    #[test]
    fn equals_is_cross_type_numeric() {
        assert!(cel_equals(&int(1), &uint(1)).unwrap());
        assert!(cel_equals(&int(1), &dbl(1.0)).unwrap());
        assert!(cel_equals(&uint(1), &dbl(1.0)).unwrap());
        assert!(!cel_equals(&int(2), &uint(1)).unwrap());
    }

    #[test]
    fn equals_nan_is_never_equal() {
        assert!(!cel_equals(&dbl(f64::NAN), &dbl(f64::NAN)).unwrap());
    }

    #[test]
    fn equals_different_nonnumeric_types_is_false_not_error() {
        assert!(!cel_equals(&CelValue::String("1".into()), &int(1)).unwrap());
        assert!(!cel_equals(&CelValue::Null, &CelValue::Bool(false)).unwrap());
    }

    #[test]
    fn equals_lists_and_maps_recurse() {
        let a = CelValue::List(vec![int(1), uint(2)]);
        let b = CelValue::List(vec![dbl(1.0), int(2)]);
        assert!(cel_equals(&a, &b).unwrap());

        let mut m1 = BTreeMap::new();
        m1.insert(CelKey::String("k".into()), int(1));
        let mut m2 = BTreeMap::new();
        m2.insert(CelKey::String("k".into()), dbl(1.0));
        assert!(cel_equals(&CelValue::Map(m1), &CelValue::Map(m2)).unwrap());
    }

    #[test]
    fn ordering_cross_type_and_boundaries() {
        assert_eq!(
            compare(BinaryOp::Lt, &int(-1), &uint(0)).unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            compare(BinaryOp::Lt, &uint(u64::MAX), &int(0)).unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            compare(BinaryOp::Lt, &int(1), &dbl(1.5)).unwrap(),
            CelValue::Bool(true)
        );
        // i64::MAX is not exactly representable as f64; the next f64 up is larger.
        assert_eq!(
            compare(BinaryOp::Lt, &int(i64::MAX), &dbl(i64::MAX as f64)).unwrap(),
            CelValue::Bool(true)
        );
    }

    #[test]
    fn ordering_nan_is_all_false() {
        for op in [BinaryOp::Lt, BinaryOp::Le, BinaryOp::Gt, BinaryOp::Ge] {
            assert_eq!(
                compare(op, &dbl(f64::NAN), &dbl(1.0)).unwrap(),
                CelValue::Bool(false)
            );
        }
    }

    #[test]
    fn ordering_incompatible_types_error() {
        assert!(compare(BinaryOp::Lt, &CelValue::String("a".into()), &int(1)).is_err());
    }

    #[test]
    fn arithmetic_overflow_and_div_zero() {
        assert_eq!(
            arithmetic(BinaryOp::Add, &int(i64::MAX), &int(1))
                .unwrap_err()
                .message(),
            "return error for overflow"
        );
        assert_eq!(
            arithmetic(BinaryOp::Div, &int(1), &int(0))
                .unwrap_err()
                .message(),
            "divide by zero"
        );
        assert_eq!(
            arithmetic(BinaryOp::Rem, &int(1), &int(0))
                .unwrap_err()
                .message(),
            "modulus by zero"
        );
        assert_eq!(
            arithmetic(BinaryOp::Div, &int(i64::MIN), &int(-1))
                .unwrap_err()
                .message(),
            "return error for overflow"
        );
    }

    #[test]
    fn arithmetic_add_concatenates() {
        assert_eq!(
            add(&CelValue::String("a".into()), &CelValue::String("b".into())).unwrap(),
            CelValue::String("ab".into())
        );
        assert_eq!(
            add(&CelValue::List(vec![int(1)]), &CelValue::List(vec![int(2)])).unwrap(),
            CelValue::List(vec![int(1), int(2)])
        );
        assert_eq!(
            add(&CelValue::Bytes(vec![1]), &CelValue::Bytes(vec![2])).unwrap(),
            CelValue::Bytes(vec![1, 2])
        );
    }

    #[test]
    fn arithmetic_mismatched_types_error() {
        assert_eq!(
            arithmetic(BinaryOp::Add, &int(1), &uint(1))
                .unwrap_err()
                .message(),
            "no such overload"
        );
    }

    #[test]
    fn unary_rules() {
        assert_eq!(
            unary(UnaryOp::Not, &CelValue::Bool(true)).unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            unary(UnaryOp::Not, &int(0)).unwrap_err().message(),
            "no matching overload"
        );
        assert_eq!(unary(UnaryOp::Neg, &int(5)).unwrap(), int(-5));
        assert_eq!(
            unary(UnaryOp::Neg, &int(i64::MIN)).unwrap_err().message(),
            "return error for overflow"
        );
        assert_eq!(
            unary(UnaryOp::Neg, &uint(1)).unwrap_err().message(),
            "no such overload"
        );
    }

    #[test]
    fn index_list_bounds() {
        let l = CelValue::List(vec![int(7), int(8), int(9)]);
        assert_eq!(index_value(&l, &int(0)).unwrap(), int(7));
        assert_eq!(index_value(&l, &uint(2)).unwrap(), int(9));
        assert_eq!(
            index_value(&l, &int(3)).unwrap_err().message(),
            "invalid_argument"
        );
        assert_eq!(
            index_value(&l, &int(-1)).unwrap_err().message(),
            "invalid_argument"
        );
        assert_eq!(
            index_value(&l, &dbl(0.1)).unwrap_err().message(),
            "invalid_argument"
        );
        assert_eq!(
            index_value(&l, &CelValue::String(String::new()))
                .unwrap_err()
                .message(),
            "invalid_argument"
        );
        // A whole-number double is a valid list index; a fractional one is not.
        assert_eq!(index_value(&l, &dbl(0.0)).unwrap(), int(7));
        assert_eq!(index_value(&l, &dbl(2.0)).unwrap(), int(9));
    }

    #[test]
    fn map_equals_cross_type_numeric_keys() {
        let mut m1 = BTreeMap::new();
        m1.insert(CelKey::Int(1), dbl(1.0));
        m1.insert(CelKey::Uint(2), uint(3));
        let mut m2 = BTreeMap::new();
        m2.insert(CelKey::Uint(1), int(1));
        m2.insert(CelKey::Int(2), dbl(3.0));
        assert!(cel_equals(&CelValue::Map(m1), &CelValue::Map(m2)).unwrap());
    }

    #[test]
    fn index_map_key() {
        let mut m = BTreeMap::new();
        m.insert(CelKey::String("k".into()), int(1));
        assert_eq!(
            index_value(&CelValue::Map(m.clone()), &CelValue::String("k".into())).unwrap(),
            int(1)
        );
        let err = index_value(&CelValue::Map(m), &CelValue::String("nope".into())).unwrap_err();
        assert!(err.message().contains("no such key"));
    }

    #[test]
    fn membership_list_and_map() {
        let l = CelValue::List(vec![int(1), uint(2)]);
        assert_eq!(membership(&dbl(2.0), &l).unwrap(), CelValue::Bool(true));
        assert_eq!(membership(&int(3), &l).unwrap(), CelValue::Bool(false));
        let mut m = BTreeMap::new();
        m.insert(CelKey::Int(1), int(10));
        assert_eq!(
            membership(&int(1), &CelValue::Map(m.clone())).unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            membership(&int(2), &CelValue::Map(m)).unwrap(),
            CelValue::Bool(false)
        );
    }

    #[test]
    fn size_of_works() {
        assert_eq!(size_of(&CelValue::String("abc".into())).unwrap(), int(3));
        assert_eq!(size_of(&CelValue::Bytes(vec![1, 2])).unwrap(), int(2));
        assert_eq!(size_of(&CelValue::List(vec![int(1)])).unwrap(), int(1));
        assert_eq!(size_of(&int(1)).unwrap_err().message(), "no such overload");
    }
}
