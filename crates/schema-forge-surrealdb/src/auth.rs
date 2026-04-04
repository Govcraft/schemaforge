//! `AuthStore` implementation for `SurrealBackend`.
//!
//! Delegates password hashing to SurrealDB's built-in `crypto::argon2` functions,
//! so the plaintext password never leaves the database.

use schema_forge_backend::error::BackendError;
use schema_forge_backend::user_store::{AuthStore, ForgeUser};
use serde::Deserialize;

use crate::SurrealBackend;

/// Internal count result for SurrealDB `SELECT count()` queries.
#[derive(Deserialize)]
struct CountResult {
    count: usize,
}

impl AuthStore for SurrealBackend {
    async fn validate_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<ForgeUser>, BackendError> {
        let mut response = self
            .client()
            .query(
                "SELECT username, roles, display_name, active FROM _forge_users \
                 WHERE username = $username \
                 AND crypto::argon2::compare(password_hash, $password) \
                 AND active = true",
            )
            .bind(("username", username.to_string()))
            .bind(("password", password.to_string()))
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("auth query failed: {e}"),
            })?;

        let users: Vec<ForgeUser> = response.take(0).map_err(|e| BackendError::QueryError {
            message: format!("auth deserialize failed: {e}"),
        })?;

        Ok(users.into_iter().next())
    }

    async fn list_users(&self) -> Result<Vec<ForgeUser>, BackendError> {
        let mut response = self
            .client()
            .query(
                "SELECT username, roles, display_name, active \
                 FROM _forge_users ORDER BY username",
            )
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user list query failed: {e}"),
            })?;

        let users: Vec<ForgeUser> = response.take(0).map_err(|e| BackendError::QueryError {
            message: format!("user list deserialize failed: {e}"),
        })?;

        Ok(users)
    }

    async fn get_user(&self, username: &str) -> Result<Option<ForgeUser>, BackendError> {
        let mut response = self
            .client()
            .query(
                "SELECT username, roles, display_name, active \
                 FROM _forge_users WHERE username = $username",
            )
            .bind(("username", username.to_string()))
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user get query failed: {e}"),
            })?;

        let users: Vec<ForgeUser> = response.take(0).map_err(|e| BackendError::QueryError {
            message: format!("user get deserialize failed: {e}"),
        })?;

        Ok(users.into_iter().next())
    }

    async fn create_user(
        &self,
        username: &str,
        password: &str,
        roles: &[String],
        display_name: &str,
    ) -> Result<(), BackendError> {
        let roles_surql = format!(
            "[{}]",
            roles
                .iter()
                .map(|r| format!("'{}'", r.replace('\'', "\\'")))
                .collect::<Vec<_>>()
                .join(", ")
        );

        self.client()
            .query(format!(
                "CREATE _forge_users SET \
                 username = $username, \
                 password_hash = crypto::argon2::generate($password), \
                 roles = {roles_surql}, \
                 display_name = $display_name, \
                 active = true"
            ))
            .bind(("username", username.to_string()))
            .bind(("password", password.to_string()))
            .bind(("display_name", display_name.to_string()))
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user create failed: {e}"),
            })?;

        Ok(())
    }

    async fn update_user(
        &self,
        username: &str,
        roles: &[String],
        display_name: &str,
    ) -> Result<(), BackendError> {
        let roles_surql = format!(
            "[{}]",
            roles
                .iter()
                .map(|r| format!("'{}'", r.replace('\'', "\\'")))
                .collect::<Vec<_>>()
                .join(", ")
        );

        self.client()
            .query(format!(
                "UPDATE _forge_users SET \
                 roles = {roles_surql}, \
                 display_name = $display_name \
                 WHERE username = $username"
            ))
            .bind(("username", username.to_string()))
            .bind(("display_name", display_name.to_string()))
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user update failed: {e}"),
            })?;

        Ok(())
    }

    async fn toggle_user_active(&self, username: &str) -> Result<(), BackendError> {
        self.client()
            .query("UPDATE _forge_users SET active = !active WHERE username = $username")
            .bind(("username", username.to_string()))
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user toggle failed: {e}"),
            })?;

        Ok(())
    }

    async fn count_users(&self) -> Result<usize, BackendError> {
        let mut response = self
            .client()
            .query("SELECT count() FROM _forge_users GROUP ALL")
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user count query failed: {e}"),
            })?;

        let count: Option<CountResult> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: format!("user count deserialize failed: {e}"),
            })?;

        Ok(count.map(|c| c.count).unwrap_or(0))
    }
}
