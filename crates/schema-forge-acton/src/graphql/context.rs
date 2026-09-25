use acton_service::middleware::Claims;

use crate::state::ForgeState;

/// Request-scoped context inserted into every async-graphql request via `.data()`.
///
/// Resolvers access it with `ctx.data::<ForgeGraphqlContext>()`.
pub struct ForgeGraphqlContext {
    pub state: ForgeState,
    /// Shared actor-backed state used by the canonical entity write handlers.
    pub app_state: acton_service::state::AppState<crate::config::SchemaForgeConfig>,
    pub claims: Option<Claims>,
}
