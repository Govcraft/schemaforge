use axum::extract::{Form, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use acton_service::prelude::{AuthSession, HtmlTemplate, TypedSession};

use crate::state::ForgeState;

use super::error::AdminError;
use super::templates::LoginTemplate;

// Re-export shared types so existing admin code continues to compile.
pub use crate::shared_auth::{ForgeUser, LoginForm};

/// Template-friendly view of the current user.
#[derive(Debug, Clone)]
pub struct CurrentUserView {
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
}

impl CurrentUserView {
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
        Some(Self {
            username,
            display_name,
            is_admin: auth.has_role("admin"),
        })
    }
}

/// Middleware: redirect to `/admin/login` if not authenticated.
pub async fn require_auth(
    auth: TypedSession<AuthSession>,
    request: Request,
    next: Next,
) -> Response {
    if auth.data().is_authenticated() {
        next.run(request).await
    } else {
        Redirect::to("/admin/login").into_response()
    }
}

/// GET /admin/login
pub async fn login_page(auth: TypedSession<AuthSession>) -> Response {
    if auth.data().is_authenticated() {
        return Redirect::to("/admin/").into_response();
    }
    HtmlTemplate::new(LoginTemplate { error: None }).into_response()
}

/// POST /admin/login
pub async fn login_submit(
    State(state): State<ForgeState>,
    mut auth: TypedSession<AuthSession>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AdminError> {
    let db = state
        .surreal_client
        .as_ref()
        .ok_or_else(|| AdminError::Internal {
            message: "SurrealDB client not configured for auth".to_string(),
        })?;

    match crate::shared_auth::validate_credentials(db, &form.username, &form.password)
        .await
        .map_err(|e| AdminError::Internal { message: e })?
    {
        Some(user) => {
            let display_name = user.display_name.unwrap_or_else(|| user.username.clone());
            auth.data_mut().login(user.username.clone(), user.roles);
            auth.data_mut().set_extra("username", user.username);
            auth.data_mut().set_extra("display_name", display_name);
            auth.save().await.map_err(|e| AdminError::Internal {
                message: format!("Session save failed: {e}"),
            })?;
            auth.regenerate().await.map_err(|e| AdminError::Internal {
                message: format!("Session regenerate failed: {e}"),
            })?;

            Ok(Redirect::to("/admin/").into_response())
        }
        None => Ok(HtmlTemplate::new(LoginTemplate {
            error: Some("Invalid username or password".to_string()),
        })
        .into_response()),
    }
}

/// POST /admin/logout
pub async fn logout(mut auth: TypedSession<AuthSession>) -> impl IntoResponse {
    auth.data_mut().logout();
    let _ = auth.save().await;
    let _ = auth.destroy().await;
    Redirect::to("/admin/login")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_view_from_unauthenticated_session() {
        let session = AuthSession::default();
        assert!(CurrentUserView::from_session(&session).is_none());
    }

    #[test]
    fn current_user_view_from_authenticated_session() {
        let mut session = AuthSession::default();
        session.login("user1".to_string(), vec!["admin".to_string()]);
        session.set_extra("username", "alice");
        session.set_extra("display_name", "Alice Smith");

        let view = CurrentUserView::from_session(&session).unwrap();
        assert_eq!(view.username, "alice");
        assert_eq!(view.display_name, "Alice Smith");
        assert!(view.is_admin);
    }

    #[test]
    fn current_user_view_non_admin() {
        let mut session = AuthSession::default();
        session.login("user2".to_string(), vec!["viewer".to_string()]);
        session.set_extra("username", "bob");

        let view = CurrentUserView::from_session(&session).unwrap();
        assert_eq!(view.username, "bob");
        assert_eq!(view.display_name, "bob");
        assert!(!view.is_admin);
    }

    #[test]
    fn current_user_view_fallback_to_user_id() {
        let mut session = AuthSession::default();
        session.login("user3".to_string(), vec![]);

        let view = CurrentUserView::from_session(&session).unwrap();
        assert_eq!(view.username, "user3");
        assert_eq!(view.display_name, "user3");
        assert!(!view.is_admin);
    }
}
