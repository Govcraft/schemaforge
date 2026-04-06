use axum::extract::{Form, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use acton_service::htmx::HxRedirect;
use acton_service::prelude::{AuthSession, TypedSession};
use acton_service::session::{FlashMessage, FlashMessages};

use crate::state::ForgeState;

use super::error::SiteError;
use super::templates::LoginTemplate;

// Re-export shared types.
pub use crate::admin::auth::CurrentUserView;
pub use crate::shared_auth::LoginForm;

/// Middleware: redirect to `/site/login` if not authenticated.
pub async fn require_auth(
    auth: TypedSession<AuthSession>,
    request: Request,
    next: Next,
) -> Response {
    if auth.data().is_authenticated() {
        next.run(request).await
    } else {
        Redirect::to("/site/login").into_response()
    }
}

/// GET /site/login
pub async fn login_page(
    State(state): State<ForgeState>,
    auth: TypedSession<AuthSession>,
) -> Response {
    if auth.data().is_authenticated() {
        return Redirect::to("/site/").into_response();
    }
    crate::template_engine::render_template(
        &state.template_engine,
        "site/login.html",
        &LoginTemplate { error: None },
    )
}

/// POST /site/login — validates credentials via HTMX.
///
/// On success: pushes a welcome flash message and returns `HxRedirect` to
/// the home page so HTMX does a full navigation (auth state changes the
/// entire page — nav bar, protected content).
///
/// On failure: returns the login card HTML fragment so HTMX swaps just the
/// form, showing the error inline without a page reload.
pub async fn login_submit(
    State(state): State<ForgeState>,
    mut auth: TypedSession<AuthSession>,
    Form(form): Form<LoginForm>,
) -> Result<Response, SiteError> {
    let auth_store = state
        .auth_store
        .as_ref()
        .ok_or_else(|| SiteError::Internal {
            message: "Auth store not configured".to_string(),
        })?;

    match auth_store
        .validate_credentials(&form.username, &form.password)
        .await
        .map_err(|e| SiteError::Internal {
            message: e.to_string(),
        })?
    {
        Some(user) => {
            let display_name = user.display_name.unwrap_or_else(|| user.username.clone());
            auth.data_mut().login(user.username.clone(), user.roles);
            auth.data_mut().set_extra("username", user.username);
            auth.data_mut()
                .set_extra("display_name", display_name.clone());
            auth.save().await.map_err(|e| SiteError::Internal {
                message: format!("Session save failed: {e}"),
            })?;
            auth.regenerate().await.map_err(|e| SiteError::Internal {
                message: format!("Session regenerate failed: {e}"),
            })?;

            let _ = FlashMessages::push(
                auth.session(),
                FlashMessage::success(format!("Welcome back, {display_name}!")),
            )
            .await;

            Ok((HxRedirect("/site/".into()), ()).into_response())
        }
        None => Ok(crate::template_engine::render_template(
            &state.template_engine,
            "site/login_card.html",
            &LoginTemplate {
                error: Some("Invalid username or password".to_string()),
            },
        )),
    }
}

/// POST /site/logout — destroys session via HTMX.
///
/// Returns `HxRedirect` to the login page so HTMX does a full navigation
/// (clears all authenticated page state).
pub async fn logout(mut auth: TypedSession<AuthSession>) -> impl IntoResponse {
    auth.data_mut().logout();
    let _ = auth.save().await;
    let _ = auth.destroy().await;
    (HxRedirect("/site/login".into()), ())
}
