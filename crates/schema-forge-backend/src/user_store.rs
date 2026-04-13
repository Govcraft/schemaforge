use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::BackendError;

/// A user record from the `_forge_users` table (without password_hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeUser {
    pub username: String,
    pub roles: Vec<String>,
    pub display_name: Option<String>,
    pub active: bool,
}

/// Storage-agnostic trait for user authentication and management.
///
/// Implementations handle:
/// - Credential validation (password hashing and comparison)
/// - User CRUD operations against the `_forge_users` table
///
/// Uses RPITIT (return position impl Trait in trait) for async methods,
/// matching the pattern used by `SchemaBackend` and `EntityStore`.
pub trait AuthStore: Send + Sync {
    /// Validate username/password credentials.
    ///
    /// Returns `Some(user)` if the credentials are valid and the user is active,
    /// `None` if credentials are invalid or the user is inactive.
    fn validate_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> impl Future<Output = Result<Option<ForgeUser>, BackendError>> + Send;

    /// List all users ordered by username.
    fn list_users(&self) -> impl Future<Output = Result<Vec<ForgeUser>, BackendError>> + Send;

    /// Get a single user by username.
    fn get_user(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Option<ForgeUser>, BackendError>> + Send;

    /// Create a new user with a plaintext password (will be hashed by the implementation).
    fn create_user(
        &self,
        username: &str,
        password: &str,
        roles: &[String],
        display_name: &str,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Update a user's roles and display name (does not change password).
    fn update_user(
        &self,
        username: &str,
        roles: &[String],
        display_name: &str,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Toggle a user's active status.
    fn toggle_user_active(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Count the total number of users.
    fn count_users(&self) -> impl Future<Output = Result<usize, BackendError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_user_deserialize() {
        let json = r#"{"username":"admin","roles":["admin"],"display_name":"Admin","active":true}"#;
        let user: ForgeUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.username, "admin");
        assert!(user.active);
        assert_eq!(user.roles, vec!["admin"]);
    }

    #[test]
    fn forge_user_serialize_roundtrip() {
        let user = ForgeUser {
            username: "alice".to_string(),
            roles: vec!["sales".to_string(), "marketing".to_string()],
            display_name: Some("Alice Chen".to_string()),
            active: true,
        };
        let json = serde_json::to_string(&user).unwrap();
        let back: ForgeUser = serde_json::from_str(&json).unwrap();
        assert_eq!(back.username, "alice");
        assert_eq!(back.roles, vec!["sales", "marketing"]);
        assert_eq!(back.display_name, Some("Alice Chen".to_string()));
    }

    // Compile-time verification that the trait has the correct bounds.
    fn _assert_auth_store_send_sync<T: AuthStore>() {}
}
