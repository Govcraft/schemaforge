use std::sync::Arc;

use axum::Router;

use crate::error::ForgeError;
use crate::routes::forge_routes;
use crate::state::{DynEntityStore, DynForgeBackend, DynSchemaBackend, ForgeState, SchemaRegistry};

/// Builder for SchemaForge's acton-service integration.
///
/// Usage:
/// ```rust,ignore
/// let extension = SchemaForgeExtension::builder()
///     .with_backend(surreal_backend)
///     .build()
///     .await?;
///
/// // Then in VersionedApiBuilder:
/// let routes = VersionedApiBuilder::<SchemaForgeConfig>::with_config()
///     .with_base_path("/api")
///     .add_version(ApiVersion::V1, |router| {
///         extension.register_routes(router)
///     })
///     .build_routes();
/// ```
pub struct SchemaForgeExtension {
    state: ForgeState,
}

/// Builder for `SchemaForgeExtension`.
pub struct SchemaForgeExtensionBuilder {
    backend: Option<Arc<dyn DynForgeBackend>>,
}

impl SchemaForgeExtensionBuilder {
    /// Create a new builder.
    fn new() -> Self {
        Self { backend: None }
    }

    /// Set the backend for schema and entity operations.
    ///
    /// The backend must implement both `SchemaBackend` and `EntityStore`.
    pub fn with_backend<B>(mut self, backend: B) -> Self
    where
        B: DynSchemaBackend + DynEntityStore + 'static,
    {
        self.backend = Some(Arc::new(backend));
        self
    }

    /// Build the `SchemaForgeExtension`.
    ///
    /// Loads existing schemas from the backend into the in-memory registry.
    /// Returns an error if no backend was provided or if loading fails.
    pub async fn build(self) -> Result<SchemaForgeExtension, ForgeError> {
        let backend = self.backend.ok_or_else(|| ForgeError::Internal {
            message: "SchemaForgeExtensionBuilder requires a backend (call .with_backend())"
                .to_string(),
        })?;

        let registry = SchemaRegistry::new();

        // Load existing schemas from the backend
        registry
            .load_from_backend(backend.as_ref())
            .await
            .map_err(ForgeError::from)?;

        let state = ForgeState { registry, backend };

        Ok(SchemaForgeExtension { state })
    }
}

impl SchemaForgeExtension {
    /// Create a new builder for `SchemaForgeExtension`.
    pub fn builder() -> SchemaForgeExtensionBuilder {
        SchemaForgeExtensionBuilder::new()
    }

    /// Register SchemaForge routes onto an existing Router.
    ///
    /// Merges the forge routes (nested under `/forge`) and applies
    /// `ForgeState` as a layer.
    pub fn register_routes<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let forge_router = forge_routes().with_state(self.state.clone());
        router.nest("/forge", forge_router)
    }

    /// Get a reference to the schema registry.
    pub fn registry(&self) -> &SchemaRegistry {
        &self.state.registry
    }

    /// Get a reference to the `ForgeState`.
    ///
    /// Useful for direct access to the state when building custom routes.
    pub fn state(&self) -> &ForgeState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_without_backend_produces_error() {
        let result = SchemaForgeExtensionBuilder::new().build().await;
        assert!(result.is_err());
        if let Err(ForgeError::Internal { message }) = result {
            assert!(message.contains("backend"));
        } else {
            panic!("expected ForgeError::Internal");
        }
    }

    // Note: Full integration tests with a real backend are in tests/integration.rs
}
