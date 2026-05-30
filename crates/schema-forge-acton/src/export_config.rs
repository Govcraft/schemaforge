//! Operator-tunable export hardening knobs and their pure decision cores.
//!
//! ADR-0003 item 9 (hardening) layers two server-side bounds on top of the
//! fail-closed export path:
//!
//! - a **configurable row cap** — the schema's `@export(max_rows)` cap can only
//!   ever *narrow* a server-wide ceiling, never widen it; and
//! - a **per-subject rate limit** — a single subject (or, for an anonymous
//!   caller, a coarse `anonymous` bucket) may only initiate so many bulk exports
//!   per fixed window, so the capability cannot be turned into a drip-feed table
//!   drain by issuing many small exports back to back.
//!
//! Both bounds are expressed as **pure, total functions** ([`resolve_max_rows`]
//! and [`RateWindow::admit`]) so the policy is unit-testable in isolation,
//! independent of the actor that holds the live rate-limit state
//! ([`crate::export_rate_limit`]).

use serde::{Deserialize, Serialize};

/// Default server-wide export row ceiling when `[schema_forge.export]` is absent.
///
/// Chosen to match the row cap used throughout the export ADR examples; an
/// operator tightens it once in config for the whole deployment. It is a
/// *ceiling*: a schema's `@export(max_rows)` may resolve lower, never higher.
pub const DEFAULT_MAX_ROWS: u64 = 100_000;

/// Default number of export initiations permitted per subject per window.
pub const DEFAULT_RATE_LIMIT_MAX_REQUESTS: u32 = 30;

/// Default length of the rate-limit window, in seconds.
pub const DEFAULT_RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// `[schema_forge.export]` section of config.toml.
///
/// Holds the server-side export hardening bounds. Defaults preserve the
/// ADR-0003 example behaviour (a 100k ceiling and a generous per-minute rate
/// limit) so an existing deployment that never wrote this section keeps working;
/// a production operator tightens the ceiling and the rate limit here once for
/// the whole instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSettings {
    /// Server-wide row ceiling applied to *every* export. The schema's
    /// `@export(max_rows)` is intersected with this value via
    /// [`resolve_max_rows`], so a schema can declare a lower cap but never one
    /// above the server ceiling. This makes the cap fail-closed: a schema author
    /// cannot widen the bound the operator set.
    #[serde(default = "default_max_rows")]
    pub default_max_rows: u64,

    /// Per-subject/tenant bulk-export rate limit.
    #[serde(default)]
    pub rate_limit: ExportRateLimitSettings,
}

/// `[schema_forge.export.rate_limit]` section of config.toml.
///
/// A fixed-window limiter: at most `max_requests` export initiations per
/// `window_secs` per subject. Setting `max_requests` to `0` disables export
/// entirely (fail-closed — no caller is admitted); leaving the section at its
/// defaults applies a generous per-minute allowance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRateLimitSettings {
    /// Maximum export initiations admitted per subject within one window.
    #[serde(default = "default_rate_limit_max_requests")]
    pub max_requests: u32,

    /// Length of the fixed window, in seconds.
    #[serde(default = "default_rate_limit_window_secs")]
    pub window_secs: u64,
}

fn default_max_rows() -> u64 {
    DEFAULT_MAX_ROWS
}

fn default_rate_limit_max_requests() -> u32 {
    DEFAULT_RATE_LIMIT_MAX_REQUESTS
}

fn default_rate_limit_window_secs() -> u64 {
    DEFAULT_RATE_LIMIT_WINDOW_SECS
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            default_max_rows: DEFAULT_MAX_ROWS,
            rate_limit: ExportRateLimitSettings::default(),
        }
    }
}

impl Default for ExportRateLimitSettings {
    fn default() -> Self {
        Self {
            max_requests: DEFAULT_RATE_LIMIT_MAX_REQUESTS,
            window_secs: DEFAULT_RATE_LIMIT_WINDOW_SECS,
        }
    }
}

/// Resolve the effective row cap for one export: the schema's declared
/// `@export(max_rows)` intersected with the server-wide `default_max_rows`
/// ceiling.
///
/// The result is the **minimum** of the two. A schema may declare a *tighter*
/// cap than the server default (e.g. a sensitive entity capped at 100 rows under
/// a 100k server ceiling), but it can never declare one *above* the server
/// ceiling — the operator's bound always wins. This is the fail-closed reading
/// the spec requires: "entity `@export max_rows` overriding a server default"
/// means the entity can override *downward*, while the server default remains an
/// un-widenable ceiling. Pure and total so the policy is unit-testable.
pub fn resolve_max_rows(entity_max_rows: u64, server_default: u64) -> u64 {
    entity_max_rows.min(server_default)
}

/// A single subject's fixed-window rate-limit accounting.
///
/// Tracks the current window's start instant (as a monotonic millisecond count
/// supplied by the caller, so the type stays clock-agnostic and testable) and
/// the number of admissions in that window. [`admit`](RateWindow::admit) is the
/// pure transition: it rolls the window when it has elapsed and decides whether
/// one more request fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateWindow {
    /// Monotonic start of the current window, in milliseconds.
    window_start_ms: u64,
    /// Admissions counted in the current window.
    count: u32,
}

impl RateWindow {
    /// Open a fresh window starting at `now_ms` with one admission already
    /// counted — the request that created the window.
    pub fn opened_at(now_ms: u64) -> Self {
        Self {
            window_start_ms: now_ms,
            count: 1,
        }
    }

    /// Decide whether a request at `now_ms` is admitted under a limit of
    /// `max_requests` per `window_ms`, returning the updated window.
    ///
    /// Fixed-window semantics:
    /// - if `now_ms` is at or past the window's end, a new window opens at
    ///   `now_ms` and this request is admitted (count resets to 1);
    /// - otherwise the request is admitted only if the current count is below
    ///   `max_requests`, incrementing the count;
    /// - a `max_requests` of `0` admits nobody (fail-closed kill switch).
    ///
    /// Pure and total: it returns `(admitted, next_window)` and never mutates
    /// shared state, so the actor that owns the live table can apply the
    /// returned window only when admission succeeds and keep the decision logic
    /// fully unit-testable.
    #[must_use]
    pub fn admit(self, now_ms: u64, max_requests: u32, window_ms: u64) -> (bool, RateWindow) {
        if max_requests == 0 {
            return (false, self);
        }
        let elapsed = now_ms.saturating_sub(self.window_start_ms);
        if elapsed >= window_ms {
            // The previous window has fully elapsed: start a new one.
            return (true, RateWindow::opened_at(now_ms));
        }
        if self.count < max_requests {
            return (
                true,
                RateWindow {
                    window_start_ms: self.window_start_ms,
                    count: self.count + 1,
                },
            );
        }
        (false, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_documented_constants() {
        let s = ExportSettings::default();
        assert_eq!(s.default_max_rows, DEFAULT_MAX_ROWS);
        assert_eq!(s.rate_limit.max_requests, DEFAULT_RATE_LIMIT_MAX_REQUESTS);
        assert_eq!(s.rate_limit.window_secs, DEFAULT_RATE_LIMIT_WINDOW_SECS);
    }

    #[test]
    fn settings_deserialize_empty_section_to_defaults() {
        let s: ExportSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.default_max_rows, DEFAULT_MAX_ROWS);
        assert_eq!(s.rate_limit.max_requests, DEFAULT_RATE_LIMIT_MAX_REQUESTS);
    }

    #[test]
    fn settings_deserialize_partial_override_keeps_other_defaults() {
        let s: ExportSettings = serde_json::from_str(r#"{ "default_max_rows": 500 }"#).unwrap();
        assert_eq!(s.default_max_rows, 500);
        // The rate-limit subsection still defaults.
        assert_eq!(s.rate_limit.max_requests, DEFAULT_RATE_LIMIT_MAX_REQUESTS);
        assert_eq!(s.rate_limit.window_secs, DEFAULT_RATE_LIMIT_WINDOW_SECS);
    }

    #[test]
    fn entity_cap_below_server_default_wins() {
        // A sensitive entity capped at 100 under a 100k server ceiling: 100 wins.
        assert_eq!(resolve_max_rows(100, 100_000), 100);
    }

    #[test]
    fn entity_cap_above_server_default_is_clamped_to_ceiling() {
        // A schema declaring a million rows cannot exceed a 1000-row server
        // ceiling — the operator's bound is un-widenable.
        assert_eq!(resolve_max_rows(1_000_000, 1_000), 1_000);
    }

    #[test]
    fn equal_caps_resolve_to_that_value() {
        assert_eq!(resolve_max_rows(50, 50), 50);
    }

    #[test]
    fn first_request_opens_window_and_is_admitted() {
        let w = RateWindow::opened_at(0);
        // The opening request is already counted.
        assert_eq!(w, RateWindow::opened_at(0));
    }

    #[test]
    fn admits_up_to_limit_within_window() {
        // limit = 3 per 1000ms window.
        let mut w = RateWindow::opened_at(0); // 1st admitted on open
        let (a2, n2) = w.admit(100, 3, 1000);
        assert!(a2);
        w = n2;
        let (a3, n3) = w.admit(200, 3, 1000);
        assert!(a3);
        w = n3;
        // 4th within the same window is rejected.
        let (a4, n4) = w.admit(300, 3, 1000);
        assert!(!a4);
        // A rejected request does not advance the window.
        assert_eq!(n4, w);
    }

    #[test]
    fn window_rolls_after_elapse_and_readmits() {
        let mut w = RateWindow::opened_at(0);
        // Exhaust the single-slot window.
        let (a2, _n2) = w.admit(100, 1, 1000);
        assert!(!a2);
        // Past the window end: a fresh window opens and admits.
        let (a3, n3) = w.admit(1000, 1, 1000);
        assert!(a3);
        w = n3;
        assert_eq!(w, RateWindow::opened_at(1000));
    }

    #[test]
    fn zero_limit_is_a_kill_switch() {
        let w = RateWindow::opened_at(0);
        let (admitted, next) = w.admit(10_000, 0, 1000);
        assert!(!admitted);
        // Even across a window boundary, zero admits nobody.
        assert_eq!(next, w);
    }

    #[test]
    fn boundary_elapsed_equal_to_window_rolls() {
        // elapsed == window_ms is treated as a new window (half-open interval).
        let w = RateWindow::opened_at(0);
        let (admitted, next) = w.admit(1000, 1, 1000);
        assert!(admitted);
        assert_eq!(next, RateWindow::opened_at(1000));
    }
}
