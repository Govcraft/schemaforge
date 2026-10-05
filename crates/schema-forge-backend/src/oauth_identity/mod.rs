//! Validated provider identities used to link external logins to local users.

mod provider_name;
mod provider_subject;

pub use provider_name::ProviderName;
pub use provider_subject::ProviderSubject;

use std::fmt;

use serde::{Deserialize, Serialize};

/// An immutable provider/subject pair. Email is never an identity-link key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderIdentity {
    /// Exact key of the configured provider.
    pub provider: ProviderName,
    /// Immutable subject reported by that provider.
    pub subject: ProviderSubject,
}

impl ProviderIdentity {
    /// Validate a configured provider name and its opaque external subject.
    pub fn new(
        provider: impl Into<String>,
        subject: impl Into<String>,
    ) -> Result<Self, IdentityError> {
        Ok(Self {
            provider: ProviderName::new(provider)?,
            subject: ProviderSubject::new(subject)?,
        })
    }

    /// Encode the pair injectively for the backend's table-wide unique field.
    ///
    /// Names are ASCII, so this length prefix also matches the system schema's
    /// computed key. Delimiters inside subjects cannot create collisions.
    pub fn storage_key(&self) -> String {
        format!(
            "{}:{}{}",
            self.provider.as_str().len(),
            self.provider,
            self.subject
        )
    }
}

/// A provider identity is malformed before storage or routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    InvalidProviderName,
    InvalidProviderSubject,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidProviderName => {
                "provider must contain 1-128 ASCII letters, digits, underscores, or hyphens"
            }
            Self::InvalidProviderSubject => {
                "provider subject must contain 1-512 bytes and no control characters"
            }
        })
    }
}

impl std::error::Error for IdentityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_key_distinguishes_pairs_with_equal_concatenation() {
        let left = ProviderIdentity::new("a", "bc:d").unwrap();
        let right = ProviderIdentity::new("ab", "c:d").unwrap();
        assert_ne!(left.storage_key(), right.storage_key());
        assert_ne!(
            ProviderIdentity::new("github", "123")
                .unwrap()
                .storage_key(),
            ProviderIdentity::new("google", "123")
                .unwrap()
                .storage_key()
        );
    }

    #[test]
    fn identity_json_roundtrips_without_normalizing_the_subject() {
        let identity = ProviderIdentity::new("github", "Opaque:Case-Sensitive").unwrap();
        let json = serde_json::to_value(&identity).unwrap();
        assert_eq!(json["provider"], "github");
        assert_eq!(json["subject"], "Opaque:Case-Sensitive");
        assert_eq!(
            serde_json::from_value::<ProviderIdentity>(json).unwrap(),
            identity
        );
    }
}
