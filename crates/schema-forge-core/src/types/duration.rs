//! Canonical string form for a `duration` value.
//!
//! A SchemaForge `duration` is a signed, nanosecond-precision span carried at
//! runtime as [`chrono::TimeDelta`]. Its canonical *string* form — used on the
//! REST wire and as a SurrealQL-friendly literal — is the Go-style notation that
//! the CEL engine's `duration()` function accepts and `string(duration)` emits:
//! a total count of seconds suffixed `s`, with an optional fractional part for
//! sub-second precision (e.g. `220752000s`, `1.5s`, `-5s`).
//!
//! [`format_go_duration`] always emits the seconds form, so the round-trip
//! `parse_go_duration(format_go_duration(d)) == d` holds for every representable
//! duration. [`parse_go_duration`] additionally accepts the `ns`, `us`/`µs`,
//! `ms`, `m`, `h`, `d`, and `w` units on input as a client convenience.
//!
//! These are pure functions with no I/O so the unit set, fractional handling,
//! and overflow behaviour are exhaustively testable without any backend.

use chrono::TimeDelta;

/// Failure modes when parsing a Go-style duration string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurationParseError {
    /// The input was empty or carried only a sign.
    Empty,
    /// A numeric group could not be parsed as a number.
    InvalidNumber {
        /// The offending text.
        group: String,
    },
    /// A unit suffix was not one of the recognised units.
    UnknownUnit {
        /// The offending unit text.
        unit: String,
    },
    /// The duration is outside the representable [`TimeDelta`] range.
    Overflow,
}

impl std::fmt::Display for DurationParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "duration string is empty"),
            Self::InvalidNumber { group } => {
                write!(f, "invalid number in duration group '{group}'")
            }
            Self::UnknownUnit { unit } => write!(
                f,
                "unknown duration unit '{unit}' (expected ns, us, ms, s, m, h, d, or w)"
            ),
            Self::Overflow => write!(f, "duration is out of the representable range"),
        }
    }
}

impl std::error::Error for DurationParseError {}

/// Nanoseconds per unit. `None` for an unrecognised unit.
fn unit_nanos(unit: &str) -> Option<f64> {
    match unit {
        "ns" => Some(1.0),
        "us" | "µs" | "μs" => Some(1_000.0),
        "ms" => Some(1_000_000.0),
        "s" => Some(1_000_000_000.0),
        "m" => Some(60_000_000_000.0),
        "h" => Some(3_600_000_000_000.0),
        "d" => Some(86_400_000_000_000.0),
        "w" => Some(604_800_000_000_000.0),
        _ => None,
    }
}

/// Render a [`TimeDelta`] as the canonical Go-style seconds string.
///
/// Whole-second values render as `{secs}s`; sub-second precision renders as
/// `{secs}.{frac}s` with trailing zeros trimmed from the fractional part.
#[must_use]
pub fn format_go_duration(d: &TimeDelta) -> String {
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

/// Parse a Go-style duration string into a [`TimeDelta`].
///
/// Accepts an optional leading `+`/`-` sign followed by one or more
/// `<number><unit>` groups. Numbers may be fractional (`1.5s`). Recognised units
/// are `ns`, `us`/`µs`/`μs`, `ms`, `s`, `m`, `h`, `d`, and `w`.
///
/// # Errors
/// Returns [`DurationParseError`] for empty input, an unparsable number, an
/// unknown unit, or a value outside the representable [`TimeDelta`] range.
pub fn parse_go_duration(s: &str) -> Result<TimeDelta, DurationParseError> {
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() {
        return Err(DurationParseError::Empty);
    }

    let mut total_nanos: i128 = 0;
    let mut chars = body.char_indices().peekable();
    let mut saw_group = false;

    while chars.peek().is_some() {
        let group_nanos = parse_one_group(body, &mut chars)?;
        total_nanos = total_nanos
            .checked_add(group_nanos)
            .ok_or(DurationParseError::Overflow)?;
        saw_group = true;
    }

    if !saw_group {
        return Err(DurationParseError::Empty);
    }
    if negative {
        total_nanos = -total_nanos;
    }

    let nanos_i64: i64 = total_nanos
        .try_into()
        .map_err(|_| DurationParseError::Overflow)?;
    Ok(TimeDelta::nanoseconds(nanos_i64))
}

/// Consume a single `<number><unit>` group and return its nanosecond value.
fn parse_one_group(
    body: &str,
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> Result<i128, DurationParseError> {
    // Consume the number: digits and an optional single '.'.
    let Some((start, _)) = chars.peek().copied() else {
        return Err(DurationParseError::Empty);
    };
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
        return Err(DurationParseError::InvalidNumber {
            group: num_str.to_string(),
        });
    }
    let value: f64 = num_str
        .parse()
        .map_err(|_| DurationParseError::InvalidNumber {
            group: num_str.to_string(),
        })?;

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
    let mult = unit_nanos(unit).ok_or_else(|| DurationParseError::UnknownUnit {
        unit: unit.to_string(),
    })?;

    Ok((value * mult).round() as i128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_units() {
        assert_eq!(
            format_go_duration(&TimeDelta::seconds(220_752_000)),
            "220752000s"
        );
        assert_eq!(format_go_duration(&TimeDelta::seconds(0)), "0s");
        assert_eq!(format_go_duration(&TimeDelta::seconds(-5)), "-5s");
    }

    #[test]
    fn format_fractional_trims_zeros() {
        let d = TimeDelta::seconds(1) + TimeDelta::milliseconds(500);
        assert_eq!(format_go_duration(&d), "1.5s");
    }

    #[test]
    fn parse_units() {
        assert_eq!(parse_go_duration("1h").unwrap(), TimeDelta::seconds(3600));
        assert_eq!(parse_go_duration("1m").unwrap(), TimeDelta::seconds(60));
        assert_eq!(parse_go_duration("5s").unwrap(), TimeDelta::seconds(5));
        assert_eq!(
            parse_go_duration("1ms").unwrap(),
            TimeDelta::milliseconds(1)
        );
        assert_eq!(
            parse_go_duration("1us").unwrap(),
            TimeDelta::microseconds(1)
        );
        assert_eq!(parse_go_duration("1ns").unwrap(), TimeDelta::nanoseconds(1));
        assert_eq!(parse_go_duration("1d").unwrap(), TimeDelta::days(1));
        assert_eq!(parse_go_duration("1w").unwrap(), TimeDelta::weeks(1));
        assert_eq!(
            parse_go_duration("2555d").unwrap(),
            TimeDelta::seconds(220_752_000)
        );
    }

    #[test]
    fn parse_compound_and_fractional() {
        assert_eq!(
            parse_go_duration("1h30m").unwrap(),
            TimeDelta::seconds(3600 + 30 * 60)
        );
        assert_eq!(
            parse_go_duration("1.5s").unwrap(),
            TimeDelta::milliseconds(1500)
        );
    }

    #[test]
    fn parse_sign() {
        assert_eq!(parse_go_duration("-5s").unwrap(), TimeDelta::seconds(-5));
        assert_eq!(parse_go_duration("+5s").unwrap(), TimeDelta::seconds(5));
    }

    #[test]
    fn parse_rejects_unknown_unit() {
        assert_eq!(
            parse_go_duration("5x"),
            Err(DurationParseError::UnknownUnit { unit: "x".into() })
        );
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(parse_go_duration(""), Err(DurationParseError::Empty));
        assert_eq!(parse_go_duration("-"), Err(DurationParseError::Empty));
    }

    #[test]
    fn parse_rejects_missing_number() {
        assert!(matches!(
            parse_go_duration("s"),
            Err(DurationParseError::UnknownUnit { .. } | DurationParseError::InvalidNumber { .. })
        ));
    }

    #[test]
    fn roundtrip_format_parse() {
        for secs in [0_i64, 1, -1, 5, 220_752_000, -220_752_000] {
            let d = TimeDelta::seconds(secs);
            assert_eq!(parse_go_duration(&format_go_duration(&d)).unwrap(), d);
        }
        let frac = TimeDelta::seconds(3) + TimeDelta::nanoseconds(123_000_000);
        assert_eq!(parse_go_duration(&format_go_duration(&frac)).unwrap(), frac);
    }

    #[test]
    fn parse_overflow_errors() {
        // Far beyond the i64-nanosecond range (~292 years).
        assert_eq!(
            parse_go_duration("100000000000000w"),
            Err(DurationParseError::Overflow)
        );
    }

    #[test]
    fn error_display_is_actionable() {
        let e = DurationParseError::UnknownUnit { unit: "x".into() };
        assert!(e.to_string().contains("ns, us, ms, s, m, h, d, or w"));
    }
}
