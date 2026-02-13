//! Shared authentication utilities for admin-ui and cloud-ui.
//!
//! Contains user validation, bootstrap logic, and shared types that both
//! the admin and cloud UIs depend on.

use serde::Deserialize;

/// SurrealDB user record (without password_hash).
#[derive(Debug, Clone, Deserialize)]
pub struct ForgeUser {
    pub username: String,
    pub roles: Vec<String>,
    pub display_name: Option<String>,
    pub active: bool,
}

/// Login form fields shared by admin and cloud auth.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

/// Validate credentials against `_forge_users` table.
///
/// Uses SurrealDB's `crypto::argon2::compare()` — the password never leaves the DB.
pub async fn validate_credentials(
    db: &schema_forge_surrealdb::surrealdb::Surreal<
        schema_forge_surrealdb::surrealdb::engine::any::Any,
    >,
    username: &str,
    password: &str,
) -> Result<Option<ForgeUser>, String> {
    let mut response = db
        .query(
            "SELECT username, roles, display_name, active FROM _forge_users \
             WHERE username = $username \
             AND crypto::argon2::compare(password_hash, $password) \
             AND active = true",
        )
        .bind(("username", username.to_string()))
        .bind(("password", password.to_string()))
        .await
        .map_err(|e| format!("Auth query failed: {e}"))?;

    let users: Vec<ForgeUser> = response
        .take(0)
        .map_err(|e| format!("Auth deserialize failed: {e}"))?;

    Ok(users.into_iter().next())
}

/// Create initial admin user if `_forge_users` table is empty.
pub async fn bootstrap_admin(
    db: &schema_forge_surrealdb::surrealdb::Surreal<
        schema_forge_surrealdb::surrealdb::engine::any::Any,
    >,
    username: &str,
    password: &str,
) -> Result<(), String> {
    #[derive(Deserialize)]
    struct CountResult {
        count: usize,
    }

    let mut response = db
        .query("SELECT count() FROM _forge_users GROUP ALL")
        .await
        .map_err(|e| format!("Bootstrap check failed: {e}"))?;

    let count: Option<CountResult> = response
        .take(0)
        .map_err(|e| format!("Bootstrap count failed: {e}"))?;

    if count.map(|c| c.count).unwrap_or(0) > 0 {
        return Ok(());
    }

    db.query(
        "CREATE _forge_users SET \
         username = $username, \
         password_hash = crypto::argon2::generate($password), \
         roles = ['admin'], \
         display_name = 'Administrator', \
         active = true",
    )
    .bind(("username", username.to_string()))
    .bind(("password", password.to_string()))
    .await
    .map_err(|e| format!("Bootstrap create failed: {e}"))?;

    Ok(())
}

/// Seed demo users when `_forge_users` has exactly 1 user (just admin).
///
/// These users map to the demo.schema `@access` annotations:
/// - alice (sales, marketing) → Contact, Company, Deal, Activity
/// - bob (hr) → Employee including salary/SSN
/// - charlie (member) → Organization, Project, Task, Document
/// - dana (finance, manager) → budget/revenue fields
/// - eve (member, manager) → manager-level write access
pub async fn bootstrap_demo_users(
    db: &schema_forge_surrealdb::surrealdb::Surreal<
        schema_forge_surrealdb::surrealdb::engine::any::Any,
    >,
) -> Result<(), String> {
    #[derive(Deserialize)]
    struct CountResult {
        count: usize,
    }

    let mut response = db
        .query("SELECT count() FROM _forge_users GROUP ALL")
        .await
        .map_err(|e| format!("Demo users check failed: {e}"))?;

    let count: Option<CountResult> = response
        .take(0)
        .map_err(|e| format!("Demo users count failed: {e}"))?;

    // Only seed when there's exactly 1 user (the bootstrapped admin)
    if count.map(|c| c.count).unwrap_or(0) != 1 {
        return Ok(());
    }

    let demo_users = [
        ("alice", "password", "['sales', 'marketing']", "Alice Chen"),
        ("bob", "password", "['hr']", "Bob Martinez"),
        ("charlie", "password", "['member']", "Charlie Kim"),
        ("dana", "password", "['finance', 'manager']", "Dana Patel"),
        ("eve", "password", "['member', 'manager']", "Eve Johnson"),
    ];

    for (username, password, roles, display_name) in demo_users {
        db.query(format!(
            "CREATE _forge_users SET \
             username = $username, \
             password_hash = crypto::argon2::generate($password), \
             roles = {roles}, \
             display_name = $display_name, \
             active = true",
        ))
        .bind(("username", username.to_string()))
        .bind(("password", password.to_string()))
        .bind(("display_name", display_name.to_string()))
        .await
        .map_err(|e| format!("Demo user '{username}' create failed: {e}"))?;
    }

    Ok(())
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
    fn login_form_deserialize() {
        let json = r#"{"username":"alice","password":"secret"}"#;
        let form: LoginForm = serde_json::from_str(json).unwrap();
        assert_eq!(form.username, "alice");
        assert_eq!(form.password, "secret");
    }
}
