//! Heap-free scalar operations shared by the evaluator and the Kani proofs.
//!
//! `None` is a range/overflow failure; callers retain CEL's canonical errors.

use chrono::{DateTime, TimeDelta, Utc};
use std::cmp::Ordering;

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| match i64::try_from(a as i128 + b as i128) { Ok(expected) => *result == Some(expected), Err(_) => result.is_none() }))]
pub(super) fn i64_add(a: i64, b: i64) -> Option<i64> {
    a.checked_add(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| match i64::try_from(a as i128 - b as i128) { Ok(expected) => *result == Some(expected), Err(_) => result.is_none() }))]
pub(super) fn i64_sub(a: i64, b: i64) -> Option<i64> {
    a.checked_sub(b)
}

// Widened i64 operands have a product in [-2^126 + 2^63, 2^126],
// so wrapping_mul in the specification is exact mathematical multiplication.
// It avoids an expensive, redundant overflow check in the proof expression.
#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| if (a as i128).wrapping_mul(b as i128) >= i64::MIN as i128 && (a as i128).wrapping_mul(b as i128) <= i64::MAX as i128 { matches!(result, Some(value) if *value as i128 == (a as i128).wrapping_mul(b as i128)) } else { result.is_none() }))]
pub(super) fn i64_mul(a: i64, b: i64) -> Option<i64> {
    a.checked_mul(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| if b == 0 || (a == i64::MIN && b == -1) { result.is_none() } else { matches!(result, Some(value) if *value == a / b) }))]
pub(super) fn i64_div(a: i64, b: i64) -> Option<i64> {
    if b == 0 || (a == i64::MIN && b == -1) {
        None
    } else {
        Some(a / b)
    }
}

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| if b == 0 || (a == i64::MIN && b == -1) { result.is_none() } else { matches!(result, Some(value) if *value == a % b) }))]
pub(super) fn i64_rem(a: i64, b: i64) -> Option<i64> {
    if b == 0 || (a == i64::MIN && b == -1) {
        None
    } else {
        Some(a % b)
    }
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| match u64::try_from(a as u128 + b as u128) { Ok(expected) => *result == Some(expected), Err(_) => result.is_none() }))]
pub(super) fn u64_add(a: u64, b: u64) -> Option<u64> {
    a.checked_add(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| if a < b { result.is_none() } else { matches!(result, Some(value) if *value as u128 == a as u128 - b as u128) }))]
pub(super) fn u64_sub(a: u64, b: u64) -> Option<u64> {
    a.checked_sub(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| if a as u128 * b as u128 >= u64::MIN as u128 && a as u128 * b as u128 <= u64::MAX as u128 { matches!(result, Some(value) if *value as u128 == a as u128 * b as u128) } else { result.is_none() }))]
pub(super) fn u64_mul(a: u64, b: u64) -> Option<u64> {
    a.checked_mul(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| if b == 0 { result.is_none() } else { matches!(result, Some(value) if *value == a / b) }))]
pub(super) fn u64_div(a: u64, b: u64) -> Option<u64> {
    a.checked_div(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| if b == 0 { result.is_none() } else { matches!(result, Some(value) if *value == a % b) }))]
pub(super) fn u64_rem(a: u64, b: u64) -> Option<u64> {
    a.checked_rem(b)
}

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| match i64::try_from(-(a as i128)) { Ok(expected) => *result == Some(expected), Err(_) => result.is_none() }))]
pub(super) fn i64_neg(a: i64) -> Option<i64> {
    a.checked_neg()
}

#[cfg_attr(kani, kani::ensures(|result: &Ordering| *result == (i as i128).cmp(&(u as i128))))]
pub(super) fn int_uint_cmp(i: i64, u: u64) -> Ordering {
    if i < 0 {
        Ordering::Less
    } else {
        (i as u64).cmp(&u)
    }
}

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| if u <= i64::MAX as u64 { matches!(result, Some(i) if *i as i128 == u as i128) } else { result.is_none() }))]
pub(super) fn uint_to_int(u: u64) -> Option<i64> {
    i64::try_from(u).ok()
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| if i >= 0 { matches!(result, Some(u) if *u as i128 == i as i128) } else { result.is_none() }))]
pub(super) fn int_to_uint(i: i64) -> Option<u64> {
    u64::try_from(i).ok()
}

#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| if d.is_finite() && d > -9_223_372_036_854_775_808.0 && d < 9_223_372_036_854_775_808.0 { matches!(result, Some(i) if *i as f64 == d.trunc()) } else { result.is_none() }))]
pub(super) fn double_to_int(d: f64) -> Option<i64> {
    let t = d.trunc();
    if !t.is_finite() || t <= -9_223_372_036_854_775_808.0 || t >= 9_223_372_036_854_775_808.0 {
        None
    } else {
        Some(t as i64)
    }
}

#[cfg_attr(kani, kani::ensures(|result: &Option<u64>| if d.is_finite() && d > -1.0 && d < 18_446_744_073_709_551_616.0 { matches!(result, Some(u) if *u as f64 == d.trunc()) } else { result.is_none() }))]
pub(super) fn double_to_uint(d: f64) -> Option<u64> {
    let t = d.trunc();
    if !(0.0..18_446_744_073_709_551_616.0).contains(&t) {
        None
    } else {
        Some(t as u64)
    }
}

#[cfg_attr(kani, kani::ensures(|result: &f64| result.is_finite() && (i < 0) == (*result < 0.0) && (*result as i128 - i as i128).abs() <= 512))]
pub(super) fn int_to_double(i: i64) -> f64 {
    i as f64
}

#[cfg_attr(kani, kani::ensures(|result: &f64| result.is_finite() && *result >= 0.0 && (*result as i128 - u as i128).abs() <= 1024))]
pub(super) fn uint_to_double(u: u64) -> f64 {
    u as f64
}

/// Exact grouping, including durations too large for an i64 nanosecond count.
#[cfg_attr(kani, kani::ensures(|result: &i128| *result - nanos as i128 == seconds as i128 * 1_000_000_000))]
pub(super) fn duration_nanos(seconds: i64, nanos: i32) -> i128 {
    seconds as i128 * 1_000_000_000 + nanos as i128
}

/// A normalized protobuf timestamp, with nonnegative subsecond nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub(super) struct TimestampParts {
    seconds: i64,
    nanos: u32,
}

/// Signed whole seconds and signed fractional nanoseconds, as exposed by Chrono.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub(super) struct DurationParts {
    seconds: i64,
    nanos: i32,
}

impl From<TimeDelta> for DurationParts {
    fn from(value: TimeDelta) -> Self {
        Self {
            seconds: value.num_seconds(),
            nanos: value.subsec_nanos(),
        }
    }
}

/// Combine durations without an intermediate fixed-width nanosecond overflow.
#[cfg_attr(kani, kani::ensures(|result: &Option<i64>| {
    let x = a.seconds as i128 * 1_000_000_000 + a.nanos as i128;
    let y = b.seconds as i128 * 1_000_000_000 + b.nanos as i128;
    let expected = if subtract { x - y } else { x + y };
    if expected >= i64::MIN as i128 && expected <= i64::MAX as i128 {
        matches!(result, Some(value) if *value as i128 == expected)
    } else { result.is_none() }
}))]
fn duration_sum(a: DurationParts, b: DurationParts, subtract: bool) -> Option<i64> {
    let a = duration_nanos(a.seconds, a.nanos);
    let b = duration_nanos(b.seconds, b.nanos);
    i64::try_from(if subtract { a - b } else { a + b }).ok()
}

/// Add/subtract canonical epoch parts and apply the CEL protobuf timestamp range.
#[cfg_attr(kani, kani::ensures(|result: &Option<TimestampParts>| {
    if timestamp.nanos >= 1_000_000_000 || duration.nanos <= -1_000_000_000 || duration.nanos >= 1_000_000_000 {
        result.is_none()
    } else {
        let seconds = if subtract { timestamp.seconds as i128 - duration.seconds as i128 } else { timestamp.seconds as i128 + duration.seconds as i128 };
        let fraction = if subtract { timestamp.nanos as i64 - duration.nanos as i64 } else { timestamp.nanos as i64 + duration.nanos as i64 };
        let carry = if fraction < 0 { -1_i128 } else if fraction >= 1_000_000_000 { 1_i128 } else { 0_i128 };
        if (-62_135_596_800..=253_402_300_799).contains(&(seconds + carry)) {
            matches!(result, Some(value) if value.seconds as i128 == seconds + carry && value.nanos < 1_000_000_000 && value.nanos as i64 == fraction - carry as i64 * 1_000_000_000)
        } else { result.is_none() }
    }
}))]
fn timestamp_sum(
    timestamp: TimestampParts,
    duration: DurationParts,
    subtract: bool,
) -> Option<TimestampParts> {
    if timestamp.nanos >= 1_000_000_000 || !(-999_999_999..=999_999_999).contains(&duration.nanos) {
        return None;
    }
    let (seconds, nanos) = if subtract {
        (
            timestamp.seconds as i128 - duration.seconds as i128,
            timestamp.nanos as i64 - duration.nanos as i64,
        )
    } else {
        (
            timestamp.seconds as i128 + duration.seconds as i128,
            timestamp.nanos as i64 + duration.nanos as i64,
        )
    };
    // Canonical fractions need at most one second of carry/borrow.
    let (carry, nanos) = match nanos {
        ..=-1 => (-1_i128, nanos + 1_000_000_000),
        1_000_000_000.. => (1_i128, nanos - 1_000_000_000),
        _ => (0_i128, nanos),
    };
    let seconds = seconds + carry;
    if !(-62_135_596_800..=253_402_300_799).contains(&seconds) {
        return None;
    }
    Some(TimestampParts {
        seconds: i64::try_from(seconds).ok()?,
        nanos: u32::try_from(nanos).ok()?,
    })
}

pub(super) fn duration_add(x: TimeDelta, y: TimeDelta) -> Option<TimeDelta> {
    duration_sum(x.into(), y.into(), false).map(TimeDelta::nanoseconds)
}

pub(super) fn duration_sub(x: TimeDelta, y: TimeDelta) -> Option<TimeDelta> {
    duration_sum(x.into(), y.into(), true).map(TimeDelta::nanoseconds)
}

pub(super) fn timestamp_add(t: DateTime<Utc>, d: TimeDelta) -> Option<DateTime<Utc>> {
    timestamp_arithmetic(t, d, false)
}

pub(super) fn timestamp_sub(t: DateTime<Utc>, d: TimeDelta) -> Option<DateTime<Utc>> {
    timestamp_arithmetic(t, d, true)
}

fn timestamp_arithmetic(t: DateTime<Utc>, d: TimeDelta, subtract: bool) -> Option<DateTime<Utc>> {
    // Preserve Chrono's legacy leap-second behavior outside protobuf's canonical
    // scalar domain. The scalar contract does not claim to verify that branch.
    if t.timestamp_subsec_nanos() >= 1_000_000_000 {
        return if subtract {
            t.checked_sub_signed(d)
        } else {
            t.checked_add_signed(d)
        }
        .filter(super::funcs::convert::ts_in_range);
    }
    let timestamp = TimestampParts {
        seconds: t.timestamp(),
        nanos: t.timestamp_subsec_nanos(),
    };
    let result = timestamp_sum(timestamp, d.into(), subtract)?;
    DateTime::from_timestamp(result.seconds, result.nanos)
}

/// Compare an `i64` against an `f64` exactly (no precision loss): widen the int to
/// `f64` only when it is exactly representable, otherwise reason via the float's
/// integer/fractional decomposition.
pub(super) fn int_double_cmp(i: i64, d: f64) -> Option<Ordering> {
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

pub(super) fn uint_double_cmp(u: u64, d: f64) -> Option<Ordering> {
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

#[cfg(kani)]
#[path = "scalar_proofs.rs"]
mod proofs;
