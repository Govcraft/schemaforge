//! Bounded delivery and reconnect settings for authenticated entity streams.
use serde::{Deserialize, Serialize};
use std::fmt;

/// `[schema_forge.events]` settings. Route exposure is explicitly opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventsConfig {
    pub enabled: bool,
    pub keep_alive_secs: u64,
    pub channel_capacity: usize,
    pub max_connections_per_user: usize,
    pub retry_secs: u64,
}
impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            keep_alive_secs: 15,
            channel_capacity: 256,
            max_connections_per_user: 8,
            retry_secs: 3,
        }
    }
}
impl EventsConfig {
    /// Refuse zero or excessive resource/timing bounds before constructing a bus.
    pub fn validate(&self) -> Result<(), EventsConfigError> {
        if !self.enabled {
            return Ok(());
        }
        if !(1..=3600).contains(&self.keep_alive_secs) {
            return Err(EventsConfigError(
                "keep_alive_secs must be between 1 and 3600",
            ));
        }
        if !(1..=65536).contains(&self.channel_capacity) {
            return Err(EventsConfigError(
                "channel_capacity must be between 1 and 65536",
            ));
        }
        if !(1..=1024).contains(&self.max_connections_per_user) {
            return Err(EventsConfigError(
                "max_connections_per_user must be between 1 and 1024",
            ));
        }
        if !(1..=3600).contains(&self.retry_secs) {
            return Err(EventsConfigError("retry_secs must be between 1 and 3600"));
        }
        Ok(())
    }
}

/// One operator-supplied stream bound is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventsConfigError(&'static str);
impl fmt::Display for EventsConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for EventsConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_bounded_and_disabled() {
        let mut config = EventsConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.channel_capacity, 256);
        assert_eq!(config.max_connections_per_user, 8);
        assert_eq!(config.keep_alive_secs, 15);
        config.enabled = true;
        assert!(config.validate().is_ok());
    }
    #[test]
    fn enabled_streams_reject_zero_and_excessive_limits() {
        for config in [
            EventsConfig {
                enabled: true,
                keep_alive_secs: 0,
                ..Default::default()
            },
            EventsConfig {
                enabled: true,
                channel_capacity: 0,
                ..Default::default()
            },
            EventsConfig {
                enabled: true,
                channel_capacity: 65537,
                ..Default::default()
            },
            EventsConfig {
                enabled: true,
                max_connections_per_user: 0,
                ..Default::default()
            },
            EventsConfig {
                enabled: true,
                retry_secs: 0,
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
    }
}
