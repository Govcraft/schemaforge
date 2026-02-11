use schema_forge_backend::auth::AuthContext;

use crate::state::ForgeState;

/// Request-scoped context inserted into every async-graphql request via `.data()`.
///
/// Resolvers access it with `ctx.data::<ForgeGraphqlContext>()`.
pub struct ForgeGraphqlContext {
    pub state: ForgeState,
    pub auth: Option<AuthContext>,
}
