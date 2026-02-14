use std::collections::BTreeMap;

use acton_service::prelude::{AuthSession, TypedSession};
use axum::body::Body;
use axum::extract::{Form, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use schema_forge_backend::auth::AuthContext;
use schema_forge_core::types::EntityId;

use crate::shared_auth::LoginForm;
use crate::state::ForgeState;

use super::handlers::{render_site, SiteError};
use super::templates::SiteLoginTemplate;

/// Template-friendly view of the current site user.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SiteUserView {
    pub username: String,
    pub display_name: String,
    pub roles: Vec<String>,
    pub is_admin: bool,
    pub avatar_url: Option<String>,
}

impl SiteUserView {
    /// Build from session data. Returns `None` if not authenticated.
    pub fn from_session(auth: &AuthSession) -> Option<Self> {
        if !auth.is_authenticated() {
            return None;
        }
        let username = auth
            .get_extra("username")
            .or_else(|| auth.user_id())
            .unwrap_or("User")
            .to_string();
        let display_name = auth
            .get_extra("display_name")
            .or_else(|| auth.get_extra("username"))
            .or_else(|| auth.user_id())
            .unwrap_or("User")
            .to_string();
        let roles = auth.roles.clone();
        Some(Self {
            username,
            display_name,
            is_admin: auth.has_role("admin"),
            roles,
            avatar_url: None,
        })
    }
}

/// Middleware: read session -> build AuthContext -> insert into extensions.
///
/// Unauthenticated requests redirect to /app/login.
/// HTMX requests get HX-Redirect header instead of 302.
pub async fn require_site_auth(
    auth_session: TypedSession<AuthSession>,
    mut request: Request,
    next: Next,
) -> Response {
    if auth_session.data().is_authenticated() {
        let auth_context = AuthContext {
            user_id: EntityId::new(),
            roles: auth_session.data().roles.clone(),
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        };
        request.extensions_mut().insert(auth_context);
        next.run(request).await
    } else {
        // Detect HTMX via HX-Request header
        let is_htmx = request.headers().get("hx-request").is_some();
        if is_htmx {
            Response::builder()
                .header("HX-Redirect", "/app/login")
                .body(Body::empty())
                .unwrap()
                .into_response()
        } else {
            Redirect::to("/app/login").into_response()
        }
    }
}

/// GET /app/login
pub async fn login_page(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
) -> Response {
    if auth.data().is_authenticated() {
        return Redirect::to("/app/").into_response();
    }
    render_site(
        &state,
        "cloud/login.html",
        &SiteLoginTemplate { error: None },
    )
}

/// POST /app/login — validates via shared_auth::validate_credentials()
pub async fn login_submit(
    State(state): State<ForgeState>,
    mut auth: TypedSession<AuthSession>,
    Form(form): Form<LoginForm>,
) -> Result<Response, SiteError> {
    let db = state.surreal_client.as_ref().ok_or_else(|| {
        SiteError::Internal("SurrealDB client not configured for auth".to_string())
    })?;

    match crate::shared_auth::validate_credentials(db, &form.username, &form.password)
        .await
        .map_err(SiteError::Internal)?
    {
        Some(user) => {
            let display_name = user.display_name.unwrap_or_else(|| user.username.clone());
            auth.data_mut().login(user.username.clone(), user.roles);
            auth.data_mut().set_extra("username", user.username);
            auth.data_mut().set_extra("display_name", display_name);
            auth.save()
                .await
                .map_err(|e| SiteError::Internal(format!("Session save failed: {e}")))?;
            auth.regenerate()
                .await
                .map_err(|e| SiteError::Internal(format!("Session regenerate failed: {e}")))?;

            Ok(Redirect::to("/app/").into_response())
        }
        None => Ok(render_site(
            &state,
            "cloud/login.html",
            &SiteLoginTemplate {
                error: Some("Invalid username or password".to_string()),
            },
        )),
    }
}

/// POST /app/logout
pub async fn logout(mut auth: TypedSession<AuthSession>) -> impl IntoResponse {
    auth.data_mut().logout();
    let _ = auth.save().await;
    let _ = auth.destroy().await;
    Redirect::to("/app/login")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_user_view_from_unauthenticated_session() {
        let session = AuthSession::default();
        assert!(SiteUserView::from_session(&session).is_none());
    }

    #[test]
    fn site_user_view_from_authenticated_session() {
        let mut session = AuthSession::default();
        session.login(
            "user1".to_string(),
            vec!["admin".to_string(), "sales".to_string()],
        );
        session.set_extra("username", "alice");
        session.set_extra("display_name", "Alice Chen");

        let view = SiteUserView::from_session(&session).unwrap();
        assert_eq!(view.username, "alice");
        assert_eq!(view.display_name, "Alice Chen");
        assert!(view.is_admin);
        assert_eq!(view.roles, vec!["admin", "sales"]);
    }

    #[test]
    fn site_user_view_non_admin() {
        let mut session = AuthSession::default();
        session.login("user2".to_string(), vec!["member".to_string()]);
        session.set_extra("username", "bob");

        let view = SiteUserView::from_session(&session).unwrap();
        assert_eq!(view.username, "bob");
        assert!(!view.is_admin);
        assert_eq!(view.roles, vec!["member"]);
    }
}
