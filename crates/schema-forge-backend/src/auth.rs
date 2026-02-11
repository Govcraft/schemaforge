use std::collections::BTreeMap;
use std::fmt;

use schema_forge_core::types::{EntityId, SchemaName};

/// Authentication and authorization context for an API request.
///
/// Extracted by the auth middleware and placed into request extensions.
/// Used by entity handlers to enforce access control.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// The authenticated user's entity ID.
    pub user_id: EntityId,
    /// Roles assigned to this user (e.g., "admin", "member").
    pub roles: Vec<String>,
    /// Tenant chain for multi-tenancy scoping.
    /// Empty until multi-tenancy is configured.
    pub tenant_chain: Vec<TenantRef>,
    /// Additional attributes from the authentication source.
    pub attributes: BTreeMap<String, String>,
}

impl AuthContext {
    /// Returns `true` if the user has the specified role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Returns `true` if the user has any of the specified roles.
    pub fn has_any_role(&self, roles: &[String]) -> bool {
        roles.iter().any(|r| self.roles.contains(r))
    }

    /// Returns `true` if the user has the "admin" role.
    pub fn is_admin(&self) -> bool {
        self.has_role("admin")
    }
}

/// A reference to a tenant entity in the hierarchy.
#[derive(Debug, Clone)]
pub struct TenantRef {
    /// The schema name of the tenant type (e.g., "Organization").
    pub schema: SchemaName,
    /// The entity ID of the specific tenant.
    pub entity_id: EntityId,
}

/// Errors that can occur during authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthError {
    /// No authentication credentials provided.
    MissingCredentials,
    /// Authentication credentials are invalid or expired.
    InvalidCredentials { reason: String },
    /// The authenticated user is inactive.
    UserInactive { user_id: String },
    /// An internal error occurred during authentication.
    Internal { message: String },
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCredentials => {
                write!(f, "no authentication credentials provided")
            }
            Self::InvalidCredentials { reason } => {
                write!(f, "invalid credentials: {reason}")
            }
            Self::UserInactive { user_id } => {
                write!(f, "user '{user_id}' is inactive")
            }
            Self::Internal { message } => {
                write!(f, "authentication error: {message}")
            }
        }
    }
}

impl std::error::Error for AuthError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(roles: Vec<&str>) -> AuthContext {
        AuthContext {
            user_id: EntityId::new(),
            roles: roles.into_iter().map(String::from).collect(),
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn has_role_returns_true_for_matching_role() {
        let ctx = make_context(vec!["admin", "member"]);
        assert!(ctx.has_role("admin"));
    }

    #[test]
    fn has_role_returns_false_for_missing_role() {
        let ctx = make_context(vec!["member"]);
        assert!(!ctx.has_role("admin"));
    }

    #[test]
    fn has_any_role_returns_true_when_one_matches() {
        let ctx = make_context(vec!["member"]);
        assert!(ctx.has_any_role(&["admin".to_string(), "member".to_string()]));
    }

    #[test]
    fn has_any_role_returns_false_when_none_match() {
        let ctx = make_context(vec!["viewer"]);
        assert!(!ctx.has_any_role(&["admin".to_string(), "member".to_string()]));
    }

    #[test]
    fn has_any_role_returns_false_for_empty_input() {
        let ctx = make_context(vec!["admin"]);
        assert!(!ctx.has_any_role(&[]));
    }

    #[test]
    fn is_admin_returns_true_with_admin_role() {
        let ctx = make_context(vec!["admin"]);
        assert!(ctx.is_admin());
    }

    #[test]
    fn is_admin_returns_false_without_admin_role() {
        let ctx = make_context(vec!["member"]);
        assert!(!ctx.is_admin());
    }

    #[test]
    fn auth_error_display_missing_credentials() {
        let err = AuthError::MissingCredentials;
        assert!(err.to_string().contains("no authentication credentials"));
    }

    #[test]
    fn auth_error_display_invalid_credentials() {
        let err = AuthError::InvalidCredentials {
            reason: "token expired".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("token expired"));
    }

    #[test]
    fn auth_error_display_user_inactive() {
        let err = AuthError::UserInactive {
            user_id: "user_123".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("user_123"));
    }

    #[test]
    fn auth_error_display_internal() {
        let err = AuthError::Internal {
            message: "db timeout".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("db timeout"));
    }

    #[test]
    fn auth_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(AuthError::MissingCredentials);
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn auth_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthError>();
    }

    #[test]
    fn auth_context_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthContext>();
    }
}
