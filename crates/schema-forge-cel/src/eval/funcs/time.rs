//! Timestamp and duration accessor built-ins.
//!
//! Timestamp accessors (`getFullYear`, `getMonth`, …) read calendar fields after
//! shifting the instant into a target time zone — either a named IANA zone
//! (resolved via `chrono-tz`, which embeds the IANA database with no native deps)
//! or a fixed `±HH:MM` offset that we parse by hand. **CEL's field-indexing
//! conventions are deliberately mixed** and are encoded here exactly as the
//! cel-spec corpus requires:
//!
//! | accessor        | basis                              |
//! |-----------------|------------------------------------|
//! | `getFullYear`   | calendar year                      |
//! | `getMonth`      | 0-based month (January = 0)        |
//! | `getDayOfMonth` | 0-based day of month (1st = 0)     |
//! | `getDate`       | 1-based day of month (1st = 1)     |
//! | `getDayOfYear`  | 0-based ordinal day (Jan 1 = 0)    |
//! | `getDayOfWeek`  | 0-based, 0 = Sunday                |
//! | `getHours`      | hour of day                        |
//! | `getMinutes`    | minute of hour                     |
//! | `getSeconds`    | second of minute                   |
//! | `getMilliseconds` | millisecond component (ns / 1e6) |
//!
//! Duration accessors instead return the TOTAL magnitude in the requested unit
//! (so `duration('10000s').getHours()` is `2`), per the cel-spec
//! `duration_converters` section.

use chrono::{DateTime, Datelike, FixedOffset, TimeDelta, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::error::EvalError;
use crate::value::CelValue;

fn overload() -> EvalError {
    EvalError::new("no such overload")
}

/// A resolved time zone: either a named IANA zone or a fixed UTC offset.
enum Zone {
    Named(Tz),
    Fixed(FixedOffset),
}

impl Zone {
    /// The calendar fields of `instant` as seen in this zone.
    fn fields(&self, instant: &DateTime<Utc>) -> Fields {
        match self {
            Self::Named(tz) => Fields::from(instant.with_timezone(tz)),
            Self::Fixed(off) => Fields::from(instant.with_timezone(off)),
        }
    }
}

/// The calendar fields read off a zoned datetime, kept zone-agnostic so the two
/// `chrono` timezone types funnel through one code path.
struct Fields {
    year: i64,
    month0: i64,
    day0: i64,
    day: i64,
    ordinal0: i64,
    weekday_from_sunday: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millis: i64,
}

impl<Tz: TimeZone> From<DateTime<Tz>> for Fields {
    fn from(dt: DateTime<Tz>) -> Self {
        Self {
            year: i64::from(dt.year()),
            month0: i64::from(dt.month0()),
            day0: i64::from(dt.day0()),
            day: i64::from(dt.day()),
            ordinal0: i64::from(dt.ordinal0()),
            weekday_from_sunday: i64::from(dt.weekday().num_days_from_sunday()),
            hour: i64::from(dt.hour()),
            minute: i64::from(dt.minute()),
            second: i64::from(dt.second()),
            millis: i64::from(dt.nanosecond() / 1_000_000),
        }
    }
}

/// Resolve the optional timezone argument. `None` → UTC; a named IANA zone; or a
/// fixed `±HH:MM` offset (a missing sign is treated as positive).
fn resolve_zone(arg: Option<&CelValue>) -> Result<Zone, EvalError> {
    match arg {
        None => Ok(Zone::Fixed(
            FixedOffset::east_opt(0).expect("zero offset is valid"),
        )),
        Some(CelValue::String(s)) => parse_zone(s),
        Some(_) => Err(overload()),
    }
}

fn parse_zone(s: &str) -> Result<Zone, EvalError> {
    if let Ok(tz) = s.parse::<Tz>() {
        return Ok(Zone::Named(tz));
    }
    parse_fixed_offset(s)
        .map(Zone::Fixed)
        .ok_or_else(|| EvalError::new("unknown timezone"))
}

/// Parse a fixed offset of the form `±HH:MM` (or `HH:MM`, taken as positive).
fn parse_fixed_offset(s: &str) -> Option<FixedOffset> {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let (h, m) = rest.split_once(':')?;
    let hours: i32 = h.parse().ok()?;
    let mins: i32 = m.parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&mins) {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + mins * 60))
}

/// Dispatch a timestamp accessor by name.
pub fn timestamp_accessor(
    ts: &DateTime<Utc>,
    name: &str,
    tz_arg: Option<&CelValue>,
) -> Result<CelValue, EvalError> {
    let f = resolve_zone(tz_arg)?.fields(ts);
    let v = match name {
        "getFullYear" => f.year,
        "getMonth" => f.month0,
        "getDayOfMonth" => f.day0,
        "getDate" => f.day,
        "getDayOfYear" => f.ordinal0,
        "getDayOfWeek" => f.weekday_from_sunday,
        "getHours" => f.hour,
        "getMinutes" => f.minute,
        "getSeconds" => f.second,
        "getMilliseconds" => f.millis,
        _ => return Err(overload()),
    };
    Ok(CelValue::Int(v))
}

/// Dispatch a duration accessor by name. Returns the TOTAL value in the unit
/// (except `getMilliseconds`, which is the sub-second millisecond component, per
/// the cel-spec note that this differs from a full conversion).
pub fn duration_accessor(d: &TimeDelta, name: &str) -> Result<CelValue, EvalError> {
    let v = match name {
        "getHours" => d.num_hours(),
        "getMinutes" => d.num_minutes(),
        "getSeconds" => d.num_seconds(),
        "getMilliseconds" => d.num_milliseconds() % 1000,
        _ => return Err(overload()),
    };
    Ok(CelValue::Int(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    fn tz(s: &str) -> CelValue {
        CelValue::String(s.into())
    }

    // 1234567890 -> Fri 2009-02-13 23:31:30 UTC
    #[test]
    fn timestamp_accessors_utc_conventions() {
        let t = ts("2009-02-13T23:31:30Z");
        assert_eq!(
            timestamp_accessor(&t, "getFullYear", None).unwrap(),
            CelValue::Int(2009)
        );
        // month is 0-based: February = 1.
        assert_eq!(
            timestamp_accessor(&t, "getMonth", None).unwrap(),
            CelValue::Int(1)
        );
        // getDayOfMonth 0-based (13th -> 12); getDate 1-based (13th -> 13).
        assert_eq!(
            timestamp_accessor(&t, "getDayOfMonth", None).unwrap(),
            CelValue::Int(12)
        );
        assert_eq!(
            timestamp_accessor(&t, "getDate", None).unwrap(),
            CelValue::Int(13)
        );
        // getDayOfYear 0-based: Feb 13 -> 43.
        assert_eq!(
            timestamp_accessor(&t, "getDayOfYear", None).unwrap(),
            CelValue::Int(43)
        );
        // getDayOfWeek 0=Sunday: Friday -> 5.
        assert_eq!(
            timestamp_accessor(&t, "getDayOfWeek", None).unwrap(),
            CelValue::Int(5)
        );
        assert_eq!(
            timestamp_accessor(&t, "getHours", None).unwrap(),
            CelValue::Int(23)
        );
        assert_eq!(
            timestamp_accessor(&t, "getMinutes", None).unwrap(),
            CelValue::Int(31)
        );
        assert_eq!(
            timestamp_accessor(&t, "getSeconds", None).unwrap(),
            CelValue::Int(30)
        );
    }

    #[test]
    fn timestamp_milliseconds_component() {
        let t = ts("2009-02-13T23:31:20.123456789Z");
        assert_eq!(
            timestamp_accessor(&t, "getMilliseconds", None).unwrap(),
            CelValue::Int(123)
        );
    }

    #[test]
    fn timestamp_named_zone_crosses_day() {
        let t = ts("2009-02-13T23:31:30Z");
        // Sydney is ahead → next day.
        assert_eq!(
            timestamp_accessor(&t, "getDate", Some(&tz("Australia/Sydney"))).unwrap(),
            CelValue::Int(14)
        );
        assert_eq!(
            timestamp_accessor(&t, "getMinutes", Some(&tz("Asia/Kathmandu"))).unwrap(),
            CelValue::Int(16)
        );
    }

    #[test]
    fn timestamp_fixed_offsets() {
        let t = ts("2009-02-13T23:31:30Z");
        // +11:00 keeps the same calendar day (0-based dayOfMonth 13).
        assert_eq!(
            timestamp_accessor(&t, "getDayOfMonth", Some(&tz("+11:00"))).unwrap(),
            CelValue::Int(13)
        );
        // 02:00 with no sign is positive.
        assert_eq!(
            timestamp_accessor(&t, "getHours", Some(&tz("02:00"))).unwrap(),
            CelValue::Int(1)
        );
        // negative offset rolls the day back.
        let t2 = ts("2009-02-13T02:00:00Z");
        assert_eq!(
            timestamp_accessor(&t2, "getDayOfMonth", Some(&tz("-02:30"))).unwrap(),
            CelValue::Int(11)
        );
        // -00:00 is UTC.
        assert_eq!(
            timestamp_accessor(&t, "getSeconds", Some(&tz("-00:00"))).unwrap(),
            CelValue::Int(30)
        );
    }

    #[test]
    fn unknown_timezone_errors() {
        let t = ts("2009-02-13T23:31:30Z");
        assert!(timestamp_accessor(&t, "getHours", Some(&tz("Not/AZone"))).is_err());
    }

    #[test]
    fn duration_accessors_are_totals() {
        assert_eq!(
            duration_accessor(&TimeDelta::seconds(10000), "getHours").unwrap(),
            CelValue::Int(2)
        );
        assert_eq!(
            duration_accessor(&TimeDelta::seconds(3730), "getMinutes").unwrap(),
            CelValue::Int(62)
        );
        assert_eq!(
            duration_accessor(&TimeDelta::seconds(3730), "getSeconds").unwrap(),
            CelValue::Int(3730)
        );
        // millisecond component, not a full conversion.
        let d = TimeDelta::seconds(123) + TimeDelta::nanoseconds(321_456_789);
        assert_eq!(
            duration_accessor(&d, "getMilliseconds").unwrap(),
            CelValue::Int(321)
        );
    }
}
