use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use super::IdentityError;

/// A provider-issued subject preserved exactly, independent of email changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProviderSubject(String);

impl ProviderSubject {
    /// Validate a value before accepting it as a provider identity component.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(IdentityError::InvalidProviderSubject);
        }
        Ok(Self(value))
    }

    /// Borrow the original value without normalization.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderSubject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ProviderSubject {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ProviderSubject {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProviderSubject {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProviderSubject> for String {
    fn from(value: ProviderSubject) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_subject_rejects_empty_control_and_oversized_values() {
        for value in ["", "  ", "a\nb", "a\0b"] {
            assert!(ProviderSubject::new(value).is_err());
            assert!(serde_json::from_value::<ProviderSubject>(serde_json::json!(value)).is_err());
        }
        assert!(ProviderSubject::new("x".repeat(513)).is_err());
        assert_eq!(
            ProviderSubject::new("Opaque-ID:ABC").unwrap().as_str(),
            "Opaque-ID:ABC"
        );
    }
}
