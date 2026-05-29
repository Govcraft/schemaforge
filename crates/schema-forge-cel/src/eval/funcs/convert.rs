//! Type-conversion built-ins: `int`, `uint`, `double`, `string`, `bytes`,
//! `bool`, `timestamp`, `duration`.
//!
//! Every conversion is a pure mapping from one [`CelValue`] to another, returning
//! the cel-spec canonical error text (matched by substring by the conformance
//! oracle) for the runtime-error cases — `"range"` for numeric/temporal
//! out-of-range, `"Type conversion error"` for an unparseable `bool` string,
//! `"invalid UTF-8"` for non-UTF-8 bytes, etc.

use chrono::{DateTime, TimeDelta, Timelike, Utc};

use crate::error::EvalError;
use crate::value::CelValue;

/// CEL bounds timestamps to the years 1..=9999 (the `google.protobuf.Timestamp`
/// range). Expressed as the inclusive UTC instants of those endpoints.
const TS_MIN_SECS: i64 = -62_135_596_800; // 0001-01-01T00:00:00Z
const TS_MAX_SECS: i64 = 253_402_300_799; // 9999-12-31T23:59:59Z

/// CEL bounds durations to ±315_576_000_000 seconds (10_000 years), matching the
/// `google.protobuf.Duration` range. Note this is *tighter* than chrono's
/// `TimeDelta` limit, so the bound must be enforced explicitly: the corpus's
/// `duration('320000000000s')` is in-range for `TimeDelta` but out-of-range for CEL.
const DURATION_MAX_SECS: i64 = 315_576_000_000;

fn range_err() -> EvalError {
    EvalError::new("range error")
}

/// Whether a timestamp instant falls inside CEL's representable range.
pub(crate) fn ts_in_range(ts: &DateTime<Utc>) -> bool {
    (TS_MIN_SECS..=TS_MAX_SECS).contains(&ts.timestamp())
}

/// Whether a duration falls inside CEL's representable range.
pub(crate) fn duration_in_range(d: &TimeDelta) -> bool {
    d.num_seconds().abs() <= DURATION_MAX_SECS
        && !(d.num_seconds().abs() == DURATION_MAX_SECS && d.subsec_nanos() != 0)
}

/// `int(x)`.
pub fn to_int(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Int(i) => Ok(CelValue::Int(*i)),
        CelValue::Uint(u) => i64::try_from(*u)
            .map(CelValue::Int)
            .map_err(|_| range_err()),
        CelValue::Double(d) => double_to_int(*d),
        CelValue::String(s) => s
            .parse::<i64>()
            .map(CelValue::Int)
            .map_err(|_| EvalError::new("Type conversion error")),
        CelValue::Timestamp(t) => Ok(CelValue::Int(t.timestamp())),
        _ => Err(EvalError::new("no such overload")),
    }
}

/// double → int with truncation toward zero and a strict range check.
///
/// The valid interval is the open-ish `(-2^63, 2^63)` evaluated in `f64`: the
/// exact double `2^63` is unrepresentable as `i64`, and the corpus also rejects
/// the exact double `-2^63` (`int(-9223372036854775808.0)` → range), so both
/// endpoints are excluded.
fn double_to_int(d: f64) -> Result<CelValue, EvalError> {
    let t = d.trunc();
    if !t.is_finite() || t <= -9_223_372_036_854_775_808.0 || t >= 9_223_372_036_854_775_808.0 {
        return Err(range_err());
    }
    Ok(CelValue::Int(t as i64))
}

/// `uint(x)`.
pub fn to_uint(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Uint(u) => Ok(CelValue::Uint(*u)),
        CelValue::Int(i) => u64::try_from(*i)
            .map(CelValue::Uint)
            .map_err(|_| range_err()),
        CelValue::Double(d) => double_to_uint(*d),
        CelValue::String(s) => s
            .parse::<u64>()
            .map(CelValue::Uint)
            .map_err(|_| EvalError::new("Type conversion error")),
        _ => Err(EvalError::new("no such overload")),
    }
}

fn double_to_uint(d: f64) -> Result<CelValue, EvalError> {
    let t = d.trunc();
    // `Range::contains` is false for NaN and for the unrepresentable upper
    // endpoint `2^64`, so this rejects NaN/±inf and out-of-range in one test.
    if !(0.0..18_446_744_073_709_551_616.0).contains(&t) {
        return Err(range_err());
    }
    Ok(CelValue::Uint(t as u64))
}

/// `double(x)`.
pub fn to_double(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Double(d) => Ok(CelValue::Double(*d)),
        CelValue::Int(i) => Ok(CelValue::Double(*i as f64)),
        CelValue::Uint(u) => Ok(CelValue::Double(*u as f64)),
        CelValue::String(s) => s
            .parse::<f64>()
            .map(CelValue::Double)
            .map_err(|_| EvalError::new("Type conversion error")),
        _ => Err(EvalError::new("no such overload")),
    }
}

/// `string(x)`.
pub fn to_string(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::String(s) => Ok(CelValue::String(s.clone())),
        CelValue::Int(i) => Ok(CelValue::String(i.to_string())),
        CelValue::Uint(u) => Ok(CelValue::String(u.to_string())),
        CelValue::Double(d) => Ok(CelValue::String(format_double(*d))),
        CelValue::Bool(b) => Ok(CelValue::String(b.to_string())),
        CelValue::Bytes(b) => String::from_utf8(b.clone())
            .map(CelValue::String)
            .map_err(|_| EvalError::new("invalid UTF-8")),
        CelValue::Timestamp(t) => Ok(CelValue::String(format_timestamp(t))),
        CelValue::Duration(d) => Ok(CelValue::String(format_duration(d))),
        _ => Err(EvalError::new("no such overload")),
    }
}

/// Render a double the way CEL/Go does for the corpus cases (`123.456`,
/// `-0.0045`). Rust's default `{}` already produces the shortest round-trippable
/// decimal without a spurious exponent for these magnitudes.
fn format_double(d: f64) -> String {
    d.to_string()
}

/// RFC3339 with a `Z` suffix and nanosecond precision only when nonzero (so a
/// whole-second timestamp renders `...:30Z`, not `...:30.000000000Z`).
fn format_timestamp(t: &DateTime<Utc>) -> String {
    if t.nanosecond() == 0 {
        t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    } else {
        // `%.9f` emits the dot + 9 digits; trim trailing zeros to match the
        // corpus (e.g. `...59.999999999Z`, `...20.123456789Z`).
        let frac = format!("{:09}", t.nanosecond());
        let frac = frac.trim_end_matches('0');
        format!("{}.{}Z", t.format("%Y-%m-%dT%H:%M:%S"), frac)
    }
}

/// Go-style duration string: total seconds with a fractional part, suffixed `s`
/// (e.g. `1000000s`, `100.5s`). The corpus only round-trips whole-second values.
fn format_duration(d: &TimeDelta) -> String {
    let total_nanos = d
        .num_nanoseconds()
        .unwrap_or_else(|| d.num_seconds() * 1_000_000_000);
    let secs = total_nanos / 1_000_000_000;
    let nanos = (total_nanos % 1_000_000_000).abs();
    if nanos == 0 {
        format!("{secs}s")
    } else {
        let frac = format!("{nanos:09}");
        let frac = frac.trim_end_matches('0');
        format!("{secs}.{frac}s")
    }
}

/// `bytes(x)`.
pub fn to_bytes(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Bytes(b) => Ok(CelValue::Bytes(b.clone())),
        CelValue::String(s) => Ok(CelValue::Bytes(s.clone().into_bytes())),
        _ => Err(EvalError::new("no such overload")),
    }
}

/// `bool(x)`. Accepts the cel-spec string spellings; any other string is a
/// `"Type conversion error"`.
pub fn to_bool(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Bool(b) => Ok(CelValue::Bool(*b)),
        CelValue::String(s) => match s.as_str() {
            "1" | "t" | "true" | "TRUE" | "True" => Ok(CelValue::Bool(true)),
            "0" | "f" | "false" | "FALSE" | "False" => Ok(CelValue::Bool(false)),
            _ => Err(EvalError::new("Type conversion error")),
        },
        _ => Err(EvalError::new("no such overload")),
    }
}

/// `timestamp(x)`.
pub fn to_timestamp(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Timestamp(t) => Ok(CelValue::Timestamp(*t)),
        CelValue::Int(secs) => DateTime::<Utc>::from_timestamp(*secs, 0)
            .filter(ts_in_range)
            .map(CelValue::Timestamp)
            .ok_or_else(range_err),
        CelValue::String(s) => parse_timestamp(s),
        _ => Err(EvalError::new("no such overload")),
    }
}

fn parse_timestamp(s: &str) -> Result<CelValue, EvalError> {
    let parsed = DateTime::parse_from_rfc3339(s).map_err(|_| EvalError::new("range error"))?;
    let utc = parsed.with_timezone(&Utc);
    if ts_in_range(&utc) {
        Ok(CelValue::Timestamp(utc))
    } else {
        Err(range_err())
    }
}

/// `duration(x)`.
pub fn to_duration(x: &CelValue) -> Result<CelValue, EvalError> {
    match x {
        CelValue::Duration(d) => Ok(CelValue::Duration(*d)),
        CelValue::String(s) => parse_go_duration(s).map(CelValue::Duration),
        _ => Err(EvalError::new("no such overload")),
    }
}

/// Parse a Go-style duration literal: an optional sign, then one or more
/// `<number><unit>` groups, with units `ns`, `us`, `ms`, `s`, `m`, `h`. Numbers
/// may be fractional (`1.5s`). The result is bounded to CEL's duration range.
///
/// Implemented as a pure function so the unit set, fractional handling, and
/// overflow/range behaviour are exhaustively testable without the evaluator.
pub fn parse_go_duration(s: &str) -> Result<TimeDelta, EvalError> {
    let conv = || EvalError::new("range error");
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() {
        return Err(conv());
    }

    let mut total_nanos: i128 = 0;
    let mut chars = body.char_indices().peekable();
    let mut saw_group = false;

    while chars.peek().is_some() {
        // Consume the number: digits and an optional single '.'.
        let start = chars.peek().map(|(i, _)| *i).unwrap();
        let mut seen_dot = false;
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if c.is_ascii_digit() || (c == '.' && !seen_dot) {
                seen_dot = seen_dot || c == '.';
                end = i + c.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let num_str = &body[start..end];
        if num_str.is_empty() || num_str == "." {
            return Err(conv());
        }
        let value: f64 = num_str.parse().map_err(|_| conv())?;

        // Consume the unit: a run of non-digit, non-dot chars.
        let unit_start = end;
        let mut unit_end = unit_start;
        while let Some(&(i, c)) = chars.peek() {
            if c.is_ascii_digit() || c == '.' {
                break;
            }
            unit_end = i + c.len_utf8();
            chars.next();
        }
        let unit = &body[unit_start..unit_end];
        let mult = unit_nanos(unit).ok_or_else(conv)?;

        // Accumulate in nanoseconds via f64 (sufficient for the corpus's
        // fractional cases) then round to the nearest nanosecond.
        let group_nanos = (value * mult).round() as i128;
        total_nanos = total_nanos.checked_add(group_nanos).ok_or_else(conv)?;
        saw_group = true;
    }

    if !saw_group {
        return Err(conv());
    }
    if negative {
        total_nanos = -total_nanos;
    }

    // Build the `TimeDelta` directly from the signed total nanoseconds; this
    // preserves the sign without juggling a separate non-negative nanos addend.
    let delta = TimeDelta::nanoseconds(total_nanos.try_into().map_err(|_| conv())?);
    if duration_in_range(&delta) {
        Ok(delta)
    } else {
        Err(conv())
    }
}

/// Nanoseconds per unit. `None` for an unrecognized unit.
fn unit_nanos(unit: &str) -> Option<f64> {
    match unit {
        "ns" => Some(1.0),
        "us" | "µs" | "μs" => Some(1_000.0),
        "ms" => Some(1_000_000.0),
        "s" => Some(1_000_000_000.0),
        "m" => Some(60_000_000_000.0),
        "h" => Some(3_600_000_000_000.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> CelValue {
        CelValue::String(v.into())
    }

    #[test]
    fn int_conversions() {
        assert_eq!(to_int(&CelValue::Uint(42)).unwrap(), CelValue::Int(42));
        assert_eq!(
            to_int(&CelValue::Uint(u64::MAX)).unwrap_err().message(),
            "range error"
        );
        assert_eq!(to_int(&CelValue::Double(1.9)).unwrap(), CelValue::Int(1));
        assert_eq!(to_int(&CelValue::Double(-7.9)).unwrap(), CelValue::Int(-7));
        assert_eq!(to_int(&s("987")).unwrap(), CelValue::Int(987));
    }

    #[test]
    fn int_double_range_boundaries() {
        // 2^63 - 1 as a double rounds up to 2^63 → out of range.
        assert!(to_int(&CelValue::Double(9_223_372_036_854_775_807.0)).is_err());
        // exact -2^63 as a double → out of range per corpus.
        assert!(to_int(&CelValue::Double(-9_223_372_036_854_775_808.0)).is_err());
        assert!(to_int(&CelValue::Double(1e99)).is_err());
        assert!(to_int(&CelValue::Double(f64::NAN)).is_err());
        assert!(to_int(&CelValue::Double(f64::INFINITY)).is_err());
        // 2^55 and 2^55+1 both representable; the latter loses precision but stays in range.
        assert_eq!(
            to_int(&CelValue::Double(36_028_797_018_963_968.0)).unwrap(),
            CelValue::Int(36_028_797_018_963_968)
        );
    }

    #[test]
    fn uint_conversions() {
        assert_eq!(to_uint(&CelValue::Int(1729)).unwrap(), CelValue::Uint(1729));
        assert_eq!(
            to_uint(&CelValue::Int(-1)).unwrap_err().message(),
            "range error"
        );
        assert_eq!(
            to_uint(&CelValue::Double(25.5)).unwrap(),
            CelValue::Uint(25)
        );
        assert!(to_uint(&CelValue::Double(6.022e23)).is_err());
        assert_eq!(to_uint(&s("300")).unwrap(), CelValue::Uint(300));
    }

    #[test]
    fn double_conversions() {
        assert_eq!(
            to_double(&CelValue::Int(1_000_000_000_000)).unwrap(),
            CelValue::Double(1e12)
        );
        assert_eq!(to_double(&s("-0.0")).unwrap(), CelValue::Double(-0.0));
        assert_eq!(
            to_double(&s("6.02214e23")).unwrap(),
            CelValue::Double(6.02214e23)
        );
    }

    #[test]
    fn string_conversions() {
        assert_eq!(to_string(&CelValue::Int(-456)).unwrap(), s("-456"));
        assert_eq!(to_string(&CelValue::Double(-4.5e-3)).unwrap(), s("-0.0045"));
        assert_eq!(
            to_string(&CelValue::Bytes(b"abc".to_vec())).unwrap(),
            s("abc")
        );
        assert_eq!(
            to_string(&CelValue::Bytes(vec![0x00, 0xff]))
                .unwrap_err()
                .message(),
            "invalid UTF-8"
        );
    }

    #[test]
    fn bool_conversions() {
        for t in ["1", "t", "true", "TRUE", "True"] {
            assert_eq!(to_bool(&s(t)).unwrap(), CelValue::Bool(true));
        }
        for f in ["0", "f", "false", "FALSE", "False"] {
            assert_eq!(to_bool(&s(f)).unwrap(), CelValue::Bool(false));
        }
        assert_eq!(
            to_bool(&s("TrUe")).unwrap_err().message(),
            "Type conversion error"
        );
    }

    #[test]
    fn timestamp_conversions_and_range() {
        let CelValue::Timestamp(t) = to_timestamp(&s("2009-02-13T23:31:30Z")).unwrap() else {
            panic!("expected timestamp");
        };
        assert_eq!(t.timestamp(), 1_234_567_890);
        assert!(to_timestamp(&s("0000-01-01T00:00:00Z")).is_err());
        assert!(to_timestamp(&s("10000-01-01T00:00:00Z")).is_err());
        assert_eq!(
            to_string(&to_timestamp(&s("9999-12-31T23:59:59.999999999Z")).unwrap()).unwrap(),
            s("9999-12-31T23:59:59.999999999Z")
        );
    }

    #[test]
    fn timestamp_to_string_whole_second_has_no_fraction() {
        assert_eq!(
            to_string(&to_timestamp(&s("2009-02-13T23:31:30Z")).unwrap()).unwrap(),
            s("2009-02-13T23:31:30Z")
        );
    }

    #[test]
    fn duration_parse_units() {
        assert_eq!(
            parse_go_duration("3600s").unwrap(),
            TimeDelta::seconds(3600)
        );
        assert_eq!(
            parse_go_duration("1h30m").unwrap(),
            TimeDelta::seconds(3600 + 30 * 60)
        );
        assert_eq!(
            parse_go_duration("1.5s").unwrap(),
            TimeDelta::milliseconds(1500)
        );
        assert_eq!(parse_go_duration("-5s").unwrap(), TimeDelta::seconds(-5));
        assert_eq!(parse_go_duration("1m").unwrap(), TimeDelta::seconds(60));
        assert_eq!(
            parse_go_duration("100ms").unwrap(),
            TimeDelta::milliseconds(100)
        );
        assert_eq!(
            parse_go_duration("10us").unwrap(),
            TimeDelta::microseconds(10)
        );
        assert_eq!(parse_go_duration("1ns").unwrap(), TimeDelta::nanoseconds(1));
    }

    #[test]
    fn duration_range_and_string_roundtrip() {
        assert!(parse_go_duration("320000000000s").is_err());
        assert!(parse_go_duration("-320000000000s").is_err());
        assert!(parse_go_duration("").is_err());
        assert!(parse_go_duration("5x").is_err());
        assert_eq!(
            to_string(&CelValue::Duration(TimeDelta::seconds(1_000_000))).unwrap(),
            s("1000000s")
        );
    }
}
