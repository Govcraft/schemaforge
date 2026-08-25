//! Supervised per-subject rate limiter for bulk export initiations (ADR-0003
//! item 9 hardening).
//!
//! Bulk export is an exfiltration surface: without a rate limit a caller who is
//! permitted to export could drain a table by issuing many small exports back to
//! back, staying under the row cap each time. This actor enforces a fixed-window
//! limit — at most `max_requests` initiations per `window_secs` per subject — so
//! the aggregate export rate is bounded, not just a single export's size.
//!
//! The live accounting (a `subject -> RateWindow` table) lives in a supervised
//! acton-reactive actor rather than ambient shared state, consistent with the
//! [`ExportJobActor`](crate::export_job::ExportJobActor). The admission *policy*
//! itself is the pure [`RateWindow::admit`](crate::export_config::RateWindow)
//! transition, so the decision logic stays unit-testable away from the actor.

use std::collections::HashMap;
use std::time::Instant;

use acton_service::prelude::*;

use crate::export_config::RateWindow;
use crate::messages::ReplyChannel;

/// Key identifying a rate-limit bucket: the authenticated subject, or a single
/// coarse `anonymous` bucket for unauthenticated callers.
///
/// Anonymous callers share one bucket on purpose: an unauthenticated export
/// flood cannot be attributed to a subject, so it is rate-limited as a whole
/// rather than handed a fresh allowance per request.
pub fn rate_limit_key(subject: Option<&str>) -> String {
    subject.unwrap_or("anonymous").to_string()
}

/// Request to admit one export initiation for `key` under the configured limit.
///
/// The reply is `true` when the request is admitted (and the subject's window
/// has been updated to count it) and `false` when the subject has exhausted its
/// allowance for the current window.
#[derive(Clone, Debug)]
pub struct AdmitExport {
    /// Rate-limit bucket key (see [`rate_limit_key`]).
    pub key: String,
    /// Maximum admissions per window for this request.
    pub max_requests: u32,
    /// Window length in milliseconds for this request.
    pub window_ms: u64,
    /// Channel the boolean admission decision is returned on.
    pub reply: ReplyChannel<bool>,
}

/// Actor owning the per-subject export rate-limit table.
///
/// State is a `subject -> RateWindow` map plus a monotonic baseline
/// ([`Instant`]) so admissions are timed against a steady clock that cannot jump
/// backwards. The admission decision is delegated to the pure
/// [`RateWindow::admit`]; the actor only persists the returned window when a
/// request is admitted.
pub struct ExportRateLimiter {
    windows: HashMap<String, RateWindow>,
    baseline: Instant,
}

impl Default for ExportRateLimiter {
    fn default() -> Self {
        Self {
            windows: HashMap::new(),
            baseline: Instant::now(),
        }
    }
}

impl std::fmt::Debug for ExportRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportRateLimiter")
            .field("tracked_subjects", &self.windows.len())
            .finish()
    }
}

impl ExportRateLimiter {
    /// Milliseconds elapsed on the monotonic clock since the actor's baseline.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.baseline.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Apply the pure admission policy to `key`, updating the stored window only
    /// on admission. Returns whether the request is admitted.
    ///
    /// Factored out as an inherent method so the table-mutation logic is unit
    /// testable against an injected `now_ms` without standing up the actor.
    fn admit(&mut self, key: &str, now_ms: u64, max_requests: u32, window_ms: u64) -> bool {
        let (admitted, next) = match self.windows.get(key) {
            Some(window) => window.admit(now_ms, max_requests, window_ms),
            // No prior window: a fresh window admits iff the limit allows at
            // least one request.
            None if max_requests > 0 => (true, RateWindow::opened_at(now_ms)),
            None => (false, RateWindow::opened_at(now_ms)),
        };
        if admitted {
            self.windows.insert(key.to_string(), next);
        }
        admitted
    }
}

impl ActorExtension for ExportRateLimiter {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        actor.mutate_on::<AdmitExport>(|actor, ctx| {
            let msg = ctx.message().clone();
            let now_ms = actor.model.now_ms();
            let admitted = actor
                .model
                .admit(&msg.key, now_ms, msg.max_requests, msg.window_ms);
            Reply::pending(async move {
                msg.reply.send(admitted).await;
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_uses_subject_when_present() {
        assert_eq!(rate_limit_key(Some("user:alice")), "user:alice");
    }

    #[test]
    fn key_falls_back_to_anonymous_bucket() {
        assert_eq!(rate_limit_key(None), "anonymous");
    }

    #[test]
    fn first_request_for_a_subject_is_admitted() {
        let mut limiter = ExportRateLimiter::default();
        assert!(limiter.admit("alice", 0, 2, 1000));
    }

    #[test]
    fn requests_beyond_limit_in_window_are_rejected() {
        let mut limiter = ExportRateLimiter::default();
        assert!(limiter.admit("alice", 0, 2, 1000)); // 1
        assert!(limiter.admit("alice", 10, 2, 1000)); // 2
        assert!(!limiter.admit("alice", 20, 2, 1000)); // 3 -> rejected
    }

    #[test]
    fn distinct_subjects_have_independent_buckets() {
        let mut limiter = ExportRateLimiter::default();
        assert!(limiter.admit("alice", 0, 1, 1000));
        assert!(!limiter.admit("alice", 10, 1, 1000));
        // bob is unaffected by alice's exhausted bucket.
        assert!(limiter.admit("bob", 10, 1, 1000));
    }

    #[test]
    fn window_resets_after_elapse() {
        let mut limiter = ExportRateLimiter::default();
        assert!(limiter.admit("alice", 0, 1, 1000));
        assert!(!limiter.admit("alice", 500, 1, 1000));
        // Past the window: re-admitted.
        assert!(limiter.admit("alice", 1000, 1, 1000));
    }

    #[test]
    fn zero_limit_rejects_even_first_request() {
        let mut limiter = ExportRateLimiter::default();
        assert!(!limiter.admit("alice", 0, 0, 1000));
    }
}
