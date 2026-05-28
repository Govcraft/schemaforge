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

/// Seed the SchemaForge demo persona accounts when the operator explicitly
/// opts in *and* the user store has exactly 1 user (just the bootstrapped
/// admin).
///
/// **Security**: this function creates five accounts whose passwords are the
/// literal string `"password"`. It is intended exclusively for the
/// `task demo` flow that exercises the bundled `demo.schema`. Production and
/// downstream deployments MUST NOT pass `seed = true`.
///
/// The `seed` flag is required at the API level — there is no implicit default
/// — so any caller that forgets to wire an opt-in path produces zero demo
/// users by construction. This is the fix for issue #53, where the demo seed
/// previously fired automatically whenever `--admin-user`/`--admin-password`
/// bootstrapped an empty backend.
///
/// These users map to the demo.schema `@access` annotations:
/// - alice (sales, marketing) → Contact, Company, Deal, Activity
/// - bob (hr) → Employee including salary/SSN
/// - charlie (member) → Organization, Project, Task, Document
/// - dana (finance, manager) → budget/revenue fields
/// - eve (member, manager) → manager-level write access
///
/// Returns `Ok(())` without mutating the store when:
/// - `seed` is `false` (the default operator-facing path), or
/// - the store does not contain exactly one user (idempotence).
pub async fn bootstrap_demo_users(
    auth_store: &dyn DynAuthStore,
    seed: bool,
) -> Result<(), String> {
    if !seed {
        return Ok(());
    }

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
    use schema_forge_backend::entity::Entity;
    use schema_forge_backend::error::BackendError;
    use schema_forge_backend::user_store::AuthStore;
    use std::sync::Mutex;

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

    /// In-memory `AuthStore` stub used only to exercise the bootstrap path.
    /// Implements only the methods `bootstrap_admin` and `bootstrap_demo_users`
    /// touch (`count_users`, `create_user`). Every other method is intentionally
    /// unimplemented so a future refactor that calls into them from the bootstrap
    /// path fails loudly at test time.
    #[derive(Default)]
    struct FakeAuthStore {
        users: Mutex<Vec<String>>,
    }

    impl FakeAuthStore {
        fn count(&self) -> usize {
            self.users.lock().expect("FakeAuthStore mutex poisoned").len()
        }

        fn usernames(&self) -> Vec<String> {
            self.users
                .lock()
                .expect("FakeAuthStore mutex poisoned")
                .clone()
        }
    }

    impl AuthStore for FakeAuthStore {
        async fn validate_credentials(
            &self,
            _username: &str,
            _password: &str,
        ) -> Result<Option<ForgeUser>, BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn list_users(&self) -> Result<Vec<ForgeUser>, BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn get_user(&self, _username: &str) -> Result<Option<ForgeUser>, BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn get_user_entity(
            &self,
            _username: &str,
        ) -> Result<Option<Entity>, BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn create_user(
            &self,
            username: &str,
            _password: &str,
            _roles: &[String],
            _display_name: &str,
        ) -> Result<(), BackendError> {
            self.users
                .lock()
                .expect("FakeAuthStore mutex poisoned")
                .push(username.to_string());
            Ok(())
        }

        async fn update_user(
            &self,
            _username: &str,
            _roles: &[String],
            _display_name: &str,
        ) -> Result<(), BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn toggle_user_active(&self, _username: &str) -> Result<(), BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn count_users(&self) -> Result<usize, BackendError> {
            Ok(self.count())
        }

        async fn delete_user(&self, _username: &str) -> Result<(), BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn change_password(
            &self,
            _username: &str,
            _new_password: &str,
        ) -> Result<(), BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn record_login(
            &self,
            _username: &str,
            _at: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn list_tenant_memberships(
            &self,
            _username: &str,
        ) -> Result<Vec<schema_forge_backend::TenantRef>, BackendError> {
            unimplemented!("not used by bootstrap path")
        }

        async fn add_tenant_membership(
            &self,
            _username: &str,
            _tenant_type: &str,
            _tenant_id: &str,
            _role: Option<&str>,
        ) -> Result<(), BackendError> {
            unimplemented!("not used by bootstrap path")
        }
    }

    /// Helper: run an async block on a single-threaded current-thread runtime.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(fut)
    }

    /// Reproduces issue #53: with admin bootstrapped via `--admin-user`/
    /// `--admin-password` and no opt-in, `bootstrap_demo_users` must leave
    /// the store with exactly one user. Pre-fix, this seeded six users.
    #[test]
    fn bootstrap_demo_users_with_seed_false_is_noop_after_admin() {
        let store = FakeAuthStore::default();

        block_on(async {
            bootstrap_admin(&store, "admin", "s3kret")
                .await
                .expect("admin bootstrap");
            bootstrap_demo_users(&store, false)
                .await
                .expect("demo bootstrap");
        });

        assert_eq!(
            store.count(),
            1,
            "default path must NOT seed demo users; \
             got users: {:?}",
            store.usernames()
        );
        assert_eq!(store.usernames(), vec!["admin".to_string()]);
    }

    /// The opt-in `task demo` flow still seeds the five demo personas.
    #[test]
    fn bootstrap_demo_users_with_seed_true_seeds_after_admin() {
        let store = FakeAuthStore::default();

        block_on(async {
            bootstrap_admin(&store, "admin", "s3kret")
                .await
                .expect("admin bootstrap");
            bootstrap_demo_users(&store, true)
                .await
                .expect("demo bootstrap");
        });

        assert_eq!(store.count(), 6, "opt-in path must seed five demo users");
        let names = store.usernames();
        for expected in ["admin", "alice", "bob", "charlie", "dana", "eve"] {
            assert!(
                names.iter().any(|u| u == expected),
                "missing {expected} in {names:?}"
            );
        }
    }

    /// Idempotence: when the store already contains more than just the admin
    /// (e.g. a re-run against a populated backend), opt-in still skips so we
    /// don't double-seed.
    #[test]
    fn bootstrap_demo_users_with_seed_true_skips_when_count_not_one() {
        let store = FakeAuthStore::default();

        block_on(async {
            bootstrap_admin(&store, "admin", "s3kret")
                .await
                .expect("admin bootstrap");
            // Pretend another user already exists.
            AuthStore::create_user(&store, "operator", "x", &[], "Operator")
                .await
                .expect("seed second user");
            bootstrap_demo_users(&store, true)
                .await
                .expect("demo bootstrap");
        });

        assert_eq!(
            store.count(),
            2,
            "opt-in must skip when the store already has >1 users"
        );
    }
}
