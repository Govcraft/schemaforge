//! Retention policy for the private invitation store.
use std::fmt;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

/// Number of days to retain ended invitations. Zero disables cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RetentionDays(u32);

/// Nonzero number of hours between invitation cleanup sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CleanupIntervalHours(NonZeroU32);

/// `[schema_forge.invites]` settings, validated during configuration loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawInvitesConfig")]
pub struct InvitesConfig {
    /// Days to keep consumed or expired invitation data. Zero disables sweeps.
    pub retention_days: RetentionDays,
    /// Positive number of hours between sweeps.
    pub cleanup_interval_hours: CleanupIntervalHours,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawInvitesConfig {
    retention_days: u32,
    cleanup_interval_hours: u32,
}
impl Default for RawInvitesConfig {
    fn default() -> Self {
        Self {
            retention_days: 30,
            cleanup_interval_hours: 24,
        }
    }
}
impl Default for InvitesConfig {
    fn default() -> Self {
        // Both constants are representable, nonzero policy defaults.
        Self {
            retention_days: RetentionDays(30),
            cleanup_interval_hours: CleanupIntervalHours(NonZeroU32::MIN.saturating_add(23)),
        }
    }
}
impl TryFrom<RawInvitesConfig> for InvitesConfig {
    type Error = InvitesConfigError;
    fn try_from(raw: RawInvitesConfig) -> Result<Self, Self::Error> {
        let hours = NonZeroU32::new(raw.cleanup_interval_hours).ok_or(InvitesConfigError(
            "cleanup_interval_hours must be greater than zero",
        ))?;
        let config = Self {
            retention_days: RetentionDays(raw.retention_days),
            cleanup_interval_hours: CleanupIntervalHours(hours),
        };
        if Instant::now().checked_add(config.interval()).is_none() {
            return Err(InvitesConfigError(
                "cleanup_interval_hours exceeds the supported timer range",
            ));
        }
        config.cutoff(Utc::now())?;
        Ok(config)
    }
}
impl InvitesConfig {
    /// Whether the deployment opted into retaining ended invitations for a finite window.
    pub fn enabled(&self) -> bool {
        self.retention_days.0 != 0
    }
    /// Time between sweeps, including disabled configurations.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.cleanup_interval_hours.0.get()) * 3600)
    }
    /// Strict retention cutoff, or `None` when cleanup is disabled.
    pub fn cutoff(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, InvitesConfigError> {
        if !self.enabled() {
            return Ok(None);
        }
        let delta = TimeDelta::try_days(i64::from(self.retention_days.0)).ok_or(
            InvitesConfigError("retention_days exceeds the supported timestamp range"),
        )?;
        now.checked_sub_signed(delta)
            .map(Some)
            .ok_or(InvitesConfigError(
                "retention_days exceeds the supported timestamp range",
            ))
    }
}

/// An invitation retention setting is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvitesConfigError(&'static str);
impl fmt::Display for InvitesConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for InvitesConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_overrides_and_disabled_retention() {
        let now = DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let defaults: InvitesConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults, InvitesConfig::default());
        assert_eq!(defaults.interval(), Duration::from_secs(24 * 3600));
        assert_eq!(
            defaults.cutoff(now).unwrap(),
            Some(now - TimeDelta::days(30))
        );
        let disabled: InvitesConfig = serde_json::from_str(r#"{"retention_days":0}"#).unwrap();
        assert!(!disabled.enabled());
        assert_eq!(disabled.cutoff(now).unwrap(), None);
        let override_config: InvitesConfig =
            serde_json::from_str(r#"{"retention_days":7,"cleanup_interval_hours":1}"#).unwrap();
        assert_eq!(
            override_config.cutoff(now).unwrap(),
            Some(now - TimeDelta::days(7))
        );
        assert_eq!(override_config.interval(), Duration::from_secs(3600));
    }
    #[test]
    fn invalid_settings_are_rejected_during_loading() {
        for raw in [
            r#"{"cleanup_interval_hours":0}"#,
            r#"{"retention_days":-1}"#,
            r#"{"retention_days":4294967295}"#,
            r#"{"retention_day":30}"#,
        ] {
            assert!(serde_json::from_str::<InvitesConfig>(raw).is_err(), "{raw}");
        }
    }
}
