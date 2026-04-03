pub mod context;
pub mod input_types;
pub mod resolvers;
pub mod schema_builder;
pub mod type_mapping;

use std::sync::Arc;

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use axum::response::{Html, IntoResponse};

use self::context::ForgeGraphqlContext;
use self::schema_builder::build_graphql_schema;
use crate::access::OptionalClaims;
use crate::state::ForgeState;

/// GraphQL POST handler.
pub async fn graphql_handler(
    State(state): State<ForgeState>,
    OptionalClaims(claims): OptionalClaims,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let schema = state.graphql_schema.load();
    let request = req.into_inner().data(ForgeGraphqlContext {
        state: state.clone(),
        claims,
    });
    schema.execute(request).await.into()
}

/// GraphiQL playground GET handler.
pub async fn graphql_playground() -> impl IntoResponse {
    Html(GraphiQLSource::build().endpoint("/forge/graphql").finish())
}

/// Rebuild the dynamic GraphQL schema from the current registry.
///
/// On success, atomically swaps the schema via ArcSwap. On failure, the old
/// schema remains active and an error is logged.
pub async fn rebuild_graphql_schema(state: &ForgeState) {
    let schemas = state.registry.list().await;
    match build_graphql_schema(&schemas) {
        Ok(s) => {
            state.graphql_schema.store(Arc::new(s));
            tracing::info!("GraphQL schema rebuilt successfully");
        }
        Err(e) => {
            tracing::error!("GraphQL schema rebuild failed: {e}");
        }
    }
}

/// Create an empty GraphQL schema wrapped for `ForgeState`.
///
/// Useful for constructing `ForgeState` in non-GraphQL contexts (e.g. AI agent)
/// when the `graphql` feature is enabled.
pub fn empty_graphql_schema() -> Arc<arc_swap::ArcSwap<async_graphql::dynamic::Schema>> {
    use async_graphql::dynamic::{Field, FieldFuture, FieldValue, Object, Schema, TypeRef};
    let query = Object::new("Query").field(Field::new(
        "_empty",
        TypeRef::named(TypeRef::BOOLEAN),
        |_ctx| FieldFuture::new(async { Ok(None::<FieldValue>) }),
    ));
    let schema = Schema::build("Query", None, None)
        .register(query)
        .finish()
        .expect("empty GraphQL schema");
    Arc::new(arc_swap::ArcSwap::new(Arc::new(schema)))
}

/// Build the initial GraphQL schema from a list of schema definitions.
///
/// Called during `SchemaForgeExtension::build()`.
pub fn build_initial_schema(
    schemas: &[schema_forge_core::types::SchemaDefinition],
) -> Result<async_graphql::dynamic::Schema, crate::error::ForgeError> {
    build_graphql_schema(schemas).map_err(|e| crate::error::ForgeError::Internal {
        message: format!("Failed to build initial GraphQL schema: {e}"),
    })
}
