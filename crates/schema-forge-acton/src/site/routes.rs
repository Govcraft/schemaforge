use std::path::PathBuf;

use axum::routing::{get, post};
use axum::Router;
use tower_http::services::ServeDir;

use crate::state::ForgeState;

use super::{auth, handlers};

/// Routes that require authentication.
fn protected_routes() -> Router<ForgeState> {
    Router::new()
        .route("/", get(handlers::home))
        // Entity CRUD
        .route(
            "/schemas/{name}/entities",
            get(handlers::entity_list).post(handlers::entity_create),
        )
        .route(
            "/schemas/{name}/entities/_table",
            get(handlers::entity_table_fragment),
        )
        .route(
            "/schemas/{name}/entities/new",
            get(handlers::entity_create_form),
        )
        .route(
            "/schemas/{name}/entities/{id}",
            get(handlers::entity_detail)
                .put(handlers::entity_update)
                .delete(handlers::entity_delete),
        )
        .route(
            "/schemas/{name}/entities/{id}/edit",
            get(handlers::entity_edit_form),
        )
}

/// Public routes that don't require authentication.
///
/// When `static_dir` is `Some`, a `ServeDir` serves files from the filesystem
/// at `/static`. Otherwise, the embedded default CSS is served at
/// `/static/site.css`.
fn public_routes(static_dir: Option<PathBuf>) -> Router<ForgeState> {
    let mut router = Router::new()
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout));

    if let Some(dir) = static_dir {
        router = router.nest_service("/static", ServeDir::new(dir));
    } else {
        router = router.route("/static/site.css", get(handlers::site_css));
    }

    router
}

/// Build the site UI router with authentication middleware.
///
/// Protected routes are wrapped with `require_auth` middleware that redirects
/// unauthenticated requests to `/site/login`. Public routes (login, logout,
/// static assets) are accessible without auth.
///
/// When `static_dir` is `Some`, static assets are served from the filesystem.
/// When `None`, the embedded default CSS is served as a fallback.
///
/// A session layer must be applied externally.
pub fn site_routes(static_dir: Option<PathBuf>) -> Router<ForgeState> {
    let protected = protected_routes().route_layer(axum::middleware::from_fn(auth::require_auth));

    protected.merge(public_routes(static_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_routes_builds() {
        // Compile-time verification that the router construction works
        let _router = site_routes(None);
    }
}
