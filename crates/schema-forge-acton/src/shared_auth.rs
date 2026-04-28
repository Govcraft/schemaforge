//! Shared authentication utilities for admin-ui and widget-ui.
//!
//! Contains bootstrap logic and shared types that both
//! the admin and widget UIs depend on.

// Re-export ForgeUser from schema-forge-backend for backward compatibility.
pub use schema_forge_backend::user_store::ForgeUser;

use serde::Deserialize;

use crate::state::DynAuthStore;

/// Login form fields shared by admin and cloud auth.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

/// Create the initial `platform_admin` user if `_forge_users` is empty.
///
/// The bootstrapped operator is the platform superuser: `platform_admin`
/// gates every user-management endpoint and bypasses schema/field/tenant
/// access checks. The literal `admin` role string is intentionally *not*
/// assigned — it is reserved for application authors to use in their
/// `@access(...)` annotations without colliding with platform privileges.
///
/// Idempotent: returns `Ok(())` without creating anything when the store
/// already has at least one user. Use [`bootstrap_admin_with_display_name`]
/// when the operator-facing display name needs to be overridden.
pub async fn bootstrap_admin(
    auth_store: &dyn DynAuthStore,
    username: &str,
    password: &str,
) -> Result<(), String> {
    bootstrap_admin_with_display_name(auth_store, username, password, "Administrator").await
}

/// Variant of [`bootstrap_admin`] that takes a custom display name.
///
/// Used by the `schemaforge bootstrap-admin` CLI subcommand so operators
/// can customize the human-readable label written into the user record.
pub async fn bootstrap_admin_with_display_name(
    auth_store: &dyn DynAuthStore,
    username: &str,
    password: &str,
    display_name: &str,
) -> Result<(), String> {
    let count = auth_store
        .count_users()
        .await
        .map_err(|e| format!("Bootstrap check failed: {e}"))?;

    if count > 0 {
        return Ok(());
    }

    let roles = vec!["platform_admin".to_string()];
    auth_store
        .create_user(username, password, &roles, display_name)
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
pub async fn bootstrap_demo_users(auth_store: &dyn DynAuthStore) -> Result<(), String> {
    let count = auth_store
        .count_users()
        .await
        .map_err(|e| format!("Demo users check failed: {e}"))?;

    // Only seed when there's exactly 1 user (the bootstrapped admin)
    if count != 1 {
        return Ok(());
    }

    let demo_users: &[(&str, &str, &[&str], &str)] = &[
        ("alice", "password", &["sales", "marketing"], "Alice Chen"),
        ("bob", "password", &["hr"], "Bob Martinez"),
        ("charlie", "password", &["member"], "Charlie Kim"),
        ("dana", "password", &["finance", "manager"], "Dana Patel"),
        ("eve", "password", &["member", "manager"], "Eve Johnson"),
    ];

    for (username, password, roles, display_name) in demo_users {
        let roles: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
        auth_store
            .create_user(username, password, &roles, display_name)
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
        let json = r#"{"username":"admin","roles":["platform_admin"],"display_name":"Admin","active":true,"role_rank":9223372036854775807}"#;
        let user: ForgeUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.username, "admin");
        assert!(user.active);
        assert_eq!(user.roles, vec!["platform_admin"]);
        assert_eq!(user.role_rank, i64::MAX);
    }

    #[test]
    fn login_form_deserialize() {
        let json = r#"{"username":"alice","password":"secret"}"#;
        let form: LoginForm = serde_json::from_str(json).unwrap();
        assert_eq!(form.username, "alice");
        assert_eq!(form.password, "secret");
    }
}
