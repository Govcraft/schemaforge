//! Operator-owned OAuth signup policy and bounded frontend return URLs.
use std::fmt;

use reqwest::Url;
use serde::{Deserialize, Serialize};

/// Account creation policy for identities without an existing link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignupPolicy {
    Open,
    #[default]
    InviteOnly,
}

/// `[schema_forge.auth]` settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthSettings {
    #[serde(default)]
    pub oauth: OAuthSettings,
}

/// `[schema_forge.auth.oauth]` settings; provider credentials stay upstream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthSettings {
    pub enabled: bool,
    pub signup: SignupPolicy,
    pub default_roles: Vec<String>,
    pub return_to_allowlist: Vec<String>,
    pub password_login: bool,
}

impl Default for OAuthSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            signup: SignupPolicy::InviteOnly,
            default_roles: vec!["member".into()],
            return_to_allowlist: Vec::new(),
            password_login: true,
        }
    }
}

impl OAuthSettings {
    /// Validate configured return targets before any routes are exposed.
    pub fn validate(&self) -> Result<(), ReturnUrlError> {
        if !self.enabled {
            return Ok(());
        }
        if self.return_to_allowlist.is_empty() {
            return Err(ReturnUrlError);
        }
        for allowed in &self.return_to_allowlist {
            let url = parse_return_url(allowed)?;
            if url.query().is_some() {
                return Err(ReturnUrlError);
            }
        }
        Ok(())
    }

    /// Compare parsed origins and path boundaries, never raw URL prefixes.
    pub fn validate_return_to(&self, candidate: &str) -> Result<ReturnUrl, ReturnUrlError> {
        let candidate = parse_return_url(candidate)?;
        let allowed = self.return_to_allowlist.iter().any(|allowed| {
            let Ok(allowed) = parse_return_url(allowed) else {
                return false;
            };
            if allowed.origin() != candidate.origin() {
                return false;
            }
            candidate.path() == allowed.path()
                || candidate
                    .path()
                    .strip_prefix(allowed.path())
                    .is_some_and(|rest| allowed.path().ends_with('/') || rest.starts_with('/'))
        });
        if !allowed {
            return Err(ReturnUrlError);
        }
        Ok(ReturnUrl(candidate))
    }
}

/// A validated frontend destination, never a provider callback URL.
#[derive(Debug, Clone)]
pub struct ReturnUrl(Url);

impl ReturnUrl {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Preserve application query parameters and replace any stale login code.
    pub fn with_login_code(self, code: &str) -> String {
        self.with_result("code", code)
    }

    /// Redirect a validated destination with a fixed callback error code.
    pub fn with_error(self, error: &str) -> String {
        self.with_result("error", error)
    }

    fn with_result(mut self, key: &str, value: &str) -> String {
        let pairs: Vec<(String, String)> = self
            .0
            .query_pairs()
            .filter(|(name, _)| name != "code" && name != "error")
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        self.0.set_query(None);
        self.0
            .query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair(key, value);
        self.0.into()
    }
}

fn parse_return_url(value: &str) -> Result<Url, ReturnUrlError> {
    if value.len() > 2048 {
        return Err(ReturnUrlError);
    }
    let url = Url::parse(value).map_err(|_| ReturnUrlError)?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let path = url.path().to_ascii_lowercase();
    // Reject path encodings that a frontend or proxy could reinterpret as
    // separators/traversal after the parsed boundary check, including nesting.
    let ambiguous = ["%2f", "%5c", "%2e", "%25"]
        .iter()
        .any(|encoded| path.contains(encoded));
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || ambiguous
        || !(url.scheme() == "https" || (url.scheme() == "http" && local))
    {
        return Err(ReturnUrlError);
    }
    Ok(url)
}

/// The requested/configured URL cannot be an OAuth return destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnUrlError;
impl fmt::Display for ReturnUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("return_to must match an allowed HTTP(S) origin and path boundary")
    }
}
impl std::error::Error for ReturnUrlError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> OAuthSettings {
        OAuthSettings {
            enabled: true,
            return_to_allowlist: vec!["https://console.example.com/app".into()],
            ..Default::default()
        }
    }

    #[test]
    fn return_url_rejects_host_path_and_encoding_bypasses() {
        for value in [
            "https://console.example.com.evil/app",
            "https://console.example.com@evil/app",
            "https://console.example.com/application",
            "https://console.example.com:444/app",
            "https://console.example.com/app/../other",
            "https://console.example.com/app/%2f../other",
            "https://console.example.com/app/%252e%252e/other",
            "javascript:alert(1)",
            "http://console.example.com/app",
            "https://console.example.com/app#fragment",
        ] {
            assert!(
                settings().validate_return_to(value).is_err(),
                "accepted {value}"
            );
        }
        assert!(settings()
            .validate_return_to("https://console.example.com/app/nested?view=1")
            .is_ok());
    }

    #[test]
    fn code_redirect_replaces_existing_code_and_preserves_query() {
        let target = settings()
            .validate_return_to("https://console.example.com/app?view=1&code=old")
            .unwrap();
        let url = Url::parse(&target.with_login_code("opaque+/code")).unwrap();
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [
                ("view".into(), "1".into()),
                ("code".into(), "opaque+/code".into())
            ]
        );
    }

    #[test]
    fn error_redirect_clears_stale_results_and_preserves_application_query() {
        let target = settings()
            .validate_return_to("https://console.example.com/app?view=1&code=old&error=old")
            .unwrap();
        let url = Url::parse(&target.clone().with_error("email_unverified")).unwrap();
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [
                ("view".into(), "1".into()),
                ("error".into(), "email_unverified".into()),
            ]
        );
        let success = target.with_login_code("new");
        assert!(!success.contains("error="));
    }

    #[test]
    fn enabled_oauth_requires_valid_allowlist_and_defaults_to_invitation() {
        assert!(OAuthSettings {
            enabled: true,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(settings().validate().is_ok());
        assert_eq!(OAuthSettings::default().signup, SignupPolicy::InviteOnly);
        assert!(OAuthSettings::default().password_login);
    }
}
