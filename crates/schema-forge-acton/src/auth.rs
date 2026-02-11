use std::future::Future;
use std::pin::Pin;

use schema_forge_backend::auth::{AuthContext, AuthError};
use schema_forge_core::types::EntityId;

/// Trait for authenticating API requests and producing an [`AuthContext`].
///
/// Uses `Pin<Box<dyn Future>>` for object safety (stored as `Arc<dyn AuthProvider>`).
///
/// Implementations should extract credentials from the request parts (e.g.,
/// `Authorization` header, cookies) and validate them against the backend.
pub trait AuthProvider: Send + Sync {
    /// Authenticate the request and produce an [`AuthContext`].
    ///
    /// Returns `Ok(AuthContext)` on success, or `Err(AuthError)` if the
    /// request cannot be authenticated.
    fn authenticate<'a>(
        &'a self,
        parts: &'a axum::http::request::Parts,
    ) -> Pin<Box<dyn Future<Output = Result<AuthContext, AuthError>> + Send + 'a>>;
}

/// An auth provider that always returns a fixed [`AuthContext`].
///
/// Useful for testing and for deployments that do not need API authentication.
pub struct NoopAuthProvider {
    roles: Vec<String>,
}

impl NoopAuthProvider {
    /// Create a new `NoopAuthProvider` that returns the given roles.
    pub fn new(roles: Vec<String>) -> Self {
        Self { roles }
    }

    /// Create a `NoopAuthProvider` with the `"admin"` role.
    pub fn admin() -> Self {
        Self::new(vec!["admin".to_string()])
    }
}

impl AuthProvider for NoopAuthProvider {
    fn authenticate<'a>(
        &'a self,
        _parts: &'a axum::http::request::Parts,
    ) -> Pin<Box<dyn Future<Output = Result<AuthContext, AuthError>> + Send + 'a>> {
        let roles = self.roles.clone();
        Box::pin(async move {
            Ok(AuthContext {
                user_id: EntityId::new(),
                roles,
                tenant_chain: Vec::new(),
                attributes: std::collections::BTreeMap::new(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build fake request parts for testing.
    fn fake_parts() -> axum::http::request::Parts {
        let (parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();
        parts
    }

    #[tokio::test]
    async fn noop_provider_returns_ok() {
        let provider = NoopAuthProvider::new(vec!["member".to_string()]);
        let parts = fake_parts();
        let result = provider.authenticate(&parts).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn noop_provider_returns_configured_roles() {
        let provider = NoopAuthProvider::new(vec!["member".to_string(), "viewer".to_string()]);
        let parts = fake_parts();
        let ctx = provider.authenticate(&parts).await.unwrap();
        assert_eq!(ctx.roles, vec!["member".to_string(), "viewer".to_string()]);
    }

    #[tokio::test]
    async fn noop_provider_admin_has_admin_role() {
        let provider = NoopAuthProvider::admin();
        let parts = fake_parts();
        let ctx = provider.authenticate(&parts).await.unwrap();
        assert!(ctx.is_admin());
    }

    #[tokio::test]
    async fn noop_provider_returns_empty_tenant_chain() {
        let provider = NoopAuthProvider::new(vec!["member".to_string()]);
        let parts = fake_parts();
        let ctx = provider.authenticate(&parts).await.unwrap();
        assert!(ctx.tenant_chain.is_empty());
    }

    #[tokio::test]
    async fn noop_provider_returns_empty_attributes() {
        let provider = NoopAuthProvider::new(vec!["member".to_string()]);
        let parts = fake_parts();
        let ctx = provider.authenticate(&parts).await.unwrap();
        assert!(ctx.attributes.is_empty());
    }
}
