use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use super::IdentityError;

/// A configured provider key that can safely occupy one URL path segment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProviderName(String);

impl ProviderName {
    /// Validate a value before accepting it as a provider identity component.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(IdentityError::InvalidProviderName);
        }
        Ok(Self(value))
    }

    /// Borrow the original value without normalization.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ProviderName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ProviderName {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProviderName {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProviderName> for String {
    fn from(value: ProviderName) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_rejects_path_injection_and_excessive_length() {
        for value in [
            "",
            "../github",
            "github/callback",
            "google?code=x",
            "a%2fb",
            "é",
        ] {
            assert!(ProviderName::new(value).is_err(), "{value}");
            assert!(serde_json::from_value::<ProviderName>(serde_json::json!(value)).is_err());
        }
        assert!(ProviderName::new("x".repeat(129)).is_err());
        assert_eq!(
            ProviderName::new("corp-oidc_1").unwrap().as_str(),
            "corp-oidc_1"
        );
    }
}
