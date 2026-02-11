use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ForgeError;
use crate::state::ForgeState;

/// Middleware that authenticates API requests using the configured [`AuthProvider`].
///
/// If `ForgeState::auth_provider` is `Some`, the middleware splits the request
/// into parts, calls `provider.authenticate(&parts)`, and on success inserts
/// the resulting [`AuthContext`] into request extensions before passing to `next`.
///
/// If `ForgeState::auth_provider` is `None`, the middleware passes the request
/// through without authentication (open access, backward compatible).
///
/// [`AuthProvider`]: crate::auth::AuthProvider
/// [`AuthContext`]: schema_forge_backend::auth::AuthContext
pub async fn auth_middleware(
    State(state): State<ForgeState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(ref provider) = state.auth_provider else {
        return next.run(request).await;
    };

    let (parts, body) = request.into_parts();

    match provider.authenticate(&parts).await {
        Ok(auth_context) => {
            let mut request = Request::from_parts(parts, body);
            request.extensions_mut().insert(auth_context);
            next.run(request).await
        }
        Err(auth_error) => {
            let forge_error: ForgeError = auth_error.into();
            forge_error.into_response()
        }
    }
}
