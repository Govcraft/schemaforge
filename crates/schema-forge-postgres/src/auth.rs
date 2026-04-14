//! `AuthStore` implementation for `PgBackend`.
//!
//! Uses the `argon2` crate for password hashing and verification, since
//! PostgreSQL does not have built-in argon2 support like SurrealDB.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::user_store::{AuthStore, ForgeUser};
use sqlx::Row;

use crate::PgBackend;

/// Internal row struct that includes the password hash for credential validation.
struct UserRow {
    username: String,
    password_hash: String,
    roles: sqlx::types::Json<Vec<String>>,
    display_name: Option<String>,
    active: bool,
}

impl PgBackend {
    /// Ensure the `_forge_users` table exists (idempotent).
    async fn ensure_auth_table(&self) -> Result<(), BackendError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS \"_forge_users\" (\
                \"username\" TEXT PRIMARY KEY, \
                \"password_hash\" TEXT NOT NULL, \
                \"roles\" JSONB NOT NULL DEFAULT '[]', \
                \"display_name\" TEXT, \
                \"active\" BOOLEAN NOT NULL DEFAULT true\
            );",
        )
        .execute(self.pool())
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("ensure auth table failed: {e}"),
        })?;

        Ok(())
    }
}

impl AuthStore for PgBackend {
    async fn validate_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<ForgeUser>, BackendError> {
        self.ensure_auth_table().await?;

        let row = sqlx::query(
            "SELECT username, password_hash, roles, display_name, active \
             FROM \"_forge_users\" WHERE username = $1 AND active = true",
        )
        .bind(username)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("auth query failed: {e}"),
        })?;

        let Some(row) = row else {
            return Ok(None);
        };

        let user_row = UserRow {
            username: row.get("username"),
            password_hash: row.get("password_hash"),
            roles: row.get("roles"),
            display_name: row.get("display_name"),
            active: row.get("active"),
        };

        // Verify the password against the stored hash
        let parsed_hash =
            PasswordHash::new(&user_row.password_hash).map_err(|e| BackendError::Internal {
                message: format!("invalid password hash format: {e}"),
            })?;

        if Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
        {
            Ok(Some(ForgeUser {
                username: user_row.username,
                roles: user_row.roles.0,
                display_name: user_row.display_name,
                active: user_row.active,
            }))
        } else {
            Ok(None)
        }
    }

    async fn list_users(&self) -> Result<Vec<ForgeUser>, BackendError> {
        self.ensure_auth_table().await?;

        let rows = sqlx::query(
            "SELECT username, roles, display_name, active \
             FROM \"_forge_users\" ORDER BY username",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("user list query failed: {e}"),
        })?;

        let users = rows
            .into_iter()
            .map(|row| {
                let roles: sqlx::types::Json<Vec<String>> = row.get("roles");
                ForgeUser {
                    username: row.get("username"),
                    roles: roles.0,
                    display_name: row.get("display_name"),
                    active: row.get("active"),
                }
            })
            .collect();

        Ok(users)
    }

    async fn get_user(&self, username: &str) -> Result<Option<ForgeUser>, BackendError> {
        self.ensure_auth_table().await?;

        let row = sqlx::query(
            "SELECT username, roles, display_name, active \
             FROM \"_forge_users\" WHERE username = $1",
        )
        .bind(username)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("user get query failed: {e}"),
        })?;

        Ok(row.map(|row| {
            let roles: sqlx::types::Json<Vec<String>> = row.get("roles");
            ForgeUser {
                username: row.get("username"),
                roles: roles.0,
                display_name: row.get("display_name"),
                active: row.get("active"),
            }
        }))
    }

    async fn create_user(
        &self,
        username: &str,
        password: &str,
        roles: &[String],
        display_name: &str,
    ) -> Result<(), BackendError> {
        self.ensure_auth_table().await?;

        let salt = SaltString::generate(&mut OsRng);
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| BackendError::Internal {
                message: format!("password hashing failed: {e}"),
            })?
            .to_string();

        let roles_json = serde_json::to_value(roles).map_err(|e| BackendError::Internal {
            message: format!("roles serialization failed: {e}"),
        })?;

        sqlx::query(
            "INSERT INTO \"_forge_users\" (username, password_hash, roles, display_name, active) \
             VALUES ($1, $2, $3, $4, true)",
        )
        .bind(username)
        .bind(&password_hash)
        .bind(&roles_json)
        .bind(display_name)
        .execute(self.pool())
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
        self.ensure_auth_table().await?;

        let roles_json = serde_json::to_value(roles).map_err(|e| BackendError::Internal {
            message: format!("roles serialization failed: {e}"),
        })?;

        sqlx::query(
            "UPDATE \"_forge_users\" SET roles = $1, display_name = $2 WHERE username = $3",
        )
        .bind(&roles_json)
        .bind(display_name)
        .bind(username)
        .execute(self.pool())
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("user update failed: {e}"),
        })?;

        Ok(())
    }

    async fn toggle_user_active(&self, username: &str) -> Result<(), BackendError> {
        self.ensure_auth_table().await?;

        sqlx::query("UPDATE \"_forge_users\" SET active = NOT active WHERE username = $1")
            .bind(username)
            .execute(self.pool())
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user toggle failed: {e}"),
            })?;

        Ok(())
    }

    async fn delete_user(&self, username: &str) -> Result<(), BackendError> {
        self.ensure_auth_table().await?;

        sqlx::query("DELETE FROM \"_forge_users\" WHERE username = $1")
            .bind(username)
            .execute(self.pool())
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user delete failed: {e}"),
            })?;

        Ok(())
    }

    async fn change_password(
        &self,
        username: &str,
        new_password: &str,
    ) -> Result<(), BackendError> {
        self.ensure_auth_table().await?;

        let salt = SaltString::generate(&mut OsRng);
        let password_hash = Argon2::default()
            .hash_password(new_password.as_bytes(), &salt)
            .map_err(|e| BackendError::Internal {
                message: format!("password hashing failed: {e}"),
            })?
            .to_string();

        sqlx::query("UPDATE \"_forge_users\" SET password_hash = $1 WHERE username = $2")
            .bind(&password_hash)
            .bind(username)
            .execute(self.pool())
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user change_password failed: {e}"),
            })?;

        Ok(())
    }

    async fn count_users(&self) -> Result<usize, BackendError> {
        self.ensure_auth_table().await?;

        let row = sqlx::query("SELECT COUNT(*) as count FROM \"_forge_users\"")
            .fetch_one(self.pool())
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("user count query failed: {e}"),
            })?;

        let count: i64 = row.get("count");
        Ok(count as usize)
    }
}
