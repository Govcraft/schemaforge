use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use acton_service::middleware::Claims;
use acton_service::prelude::{AuthSession, TypedSession};

/// Middleware that bridges session-based authentication to `Claims` in request extensions.
///
/// When a user is authenticated via a session cookie (e.g. after logging in through
/// the admin UI), this middleware constructs a `Claims` value from the session data
/// and inserts it into request extensions. The existing `OptionalClaims` extractor
/// then picks it up transparently — no handler changes required.
///
/// If `Claims` already exist in extensions (injected by the upstream PASETO token
/// middleware), the session is ignored so token-based API access continues to work.
pub async fn session_to_claims(
    auth: TypedSession<AuthSession>,
    mut request: Request,
    next: Next,
) -> Response {
    // PASETO token already provided Claims — don't override.
    if request.extensions().get::<Claims>().is_some() {
        return next.run(request).await;
    }

    let session = auth.data();
    if session.is_authenticated() {
        if let Some(user_id) = session.user_id() {
            let claims = Claims {
                sub: format!("user:{user_id}"),
                roles: session.roles.clone(),
                perms: vec![],
                exp: 9_999_999_999,
                iat: None,
                jti: None,
                iss: Some("schemaforge-session".to_string()),
                aud: None,
                email: None,
                username: session.get_extra("username").map(String::from),
                custom: std::collections::HashMap::new(),
            };
            request.extensions_mut().insert(claims);
        }
    }

    next.run(request).await
}
