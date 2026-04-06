use acton_service::session::FlashMessage;

use crate::admin::auth::CurrentUserView;
use crate::views::{EntityView, FieldView, PaginationView, SchemaView};

/// Serializable flash message for MiniJinja templates.
#[derive(serde::Serialize)]
pub struct FlashView {
    pub message: String,
    pub css_class: String,
}

impl FlashView {
    pub fn from_flash_messages(messages: Vec<FlashMessage>) -> Vec<Self> {
        messages
            .into_iter()
            .map(|m| Self {
                message: m.message,
                css_class: m.kind.css_class().to_string(),
            })
            .collect()
    }
}

/// Login page — standalone, no base.html.
#[derive(serde::Serialize)]
pub struct LoginTemplate {
    pub error: Option<String>,
}

/// Schema summary for the home page.
#[derive(serde::Serialize)]
pub struct SchemaSummary {
    pub name: String,
    pub field_count: usize,
}

/// Home page — lists available schemas.
#[derive(serde::Serialize)]
pub struct HomeTemplate {
    pub schemas: Vec<SchemaSummary>,
    pub current_user: Option<CurrentUserView>,
    pub flash: Vec<FlashView>,
}

/// Entity list page — paginated table within site layout.
#[derive(serde::Serialize)]
pub struct EntityListTemplate {
    pub schema: SchemaView,
    pub entities: Vec<EntityView>,
    pub pagination: PaginationView,
    pub current_user: Option<CurrentUserView>,
    pub url_prefix: String,
    pub flash: Vec<FlashView>,
}

/// Entity detail page — single entity within site layout.
#[derive(serde::Serialize)]
pub struct EntityDetailTemplate {
    pub schema: SchemaView,
    pub entity: EntityView,
    pub current_user: Option<CurrentUserView>,
    pub url_prefix: String,
    pub flash: Vec<FlashView>,
}

/// Entity create/edit form page within site layout.
#[derive(serde::Serialize)]
pub struct EntityFormTemplate {
    pub schema: SchemaView,
    pub fields: Vec<FieldView>,
    pub entity_id: Option<String>,
    pub errors: Vec<String>,
    pub current_user: Option<CurrentUserView>,
    pub url_prefix: String,
    pub flash: Vec<FlashView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_template_serializes() {
        let tmpl = LoginTemplate {
            error: Some("bad password".into()),
        };
        let json = serde_json::to_value(&tmpl).unwrap();
        assert_eq!(json["error"], "bad password");
    }

    #[test]
    fn home_template_serializes() {
        let tmpl = HomeTemplate {
            schemas: vec![SchemaSummary {
                name: "Contact".into(),
                field_count: 3,
            }],
            current_user: None,
            flash: vec![],
        };
        let json = serde_json::to_value(&tmpl).unwrap();
        assert_eq!(json["schemas"][0]["name"], "Contact");
        assert_eq!(json["schemas"][0]["field_count"], 3);
    }
}
