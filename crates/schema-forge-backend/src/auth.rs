use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use schema_forge_core::types::{
    DynamicValue, EntityId, FieldAnnotation, SchemaDefinition, SchemaName,
};

use crate::entity::Entity;

/// Authentication and authorization context for an API request.
///
/// Extracted by the auth middleware and placed into request extensions.
/// Used by entity handlers to enforce access control.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// The authenticated user's entity ID.
    pub user_id: EntityId,
    /// Roles assigned to this user (e.g., "admin", "member").
    pub roles: Vec<String>,
    /// Tenant chain for multi-tenancy scoping.
    /// Empty until multi-tenancy is configured.
    pub tenant_chain: Vec<TenantRef>,
    /// Additional attributes from the authentication source.
    pub attributes: BTreeMap<String, String>,
}

impl AuthContext {
    /// Returns `true` if the user has the specified role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Returns `true` if the user has any of the specified roles.
    pub fn has_any_role(&self, roles: &[String]) -> bool {
        roles.iter().any(|r| self.roles.contains(r))
    }

    /// Returns `true` if the user has the "admin" role.
    pub fn is_admin(&self) -> bool {
        self.has_role("admin")
    }
}

/// A reference to a tenant entity in the hierarchy.
#[derive(Debug, Clone)]
pub struct TenantRef {
    /// The schema name of the tenant type (e.g., "Organization").
    pub schema: SchemaName,
    /// The entity ID of the specific tenant.
    pub entity_id: EntityId,
}

/// Errors that can occur during authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthError {
    /// No authentication credentials provided.
    MissingCredentials,
    /// Authentication credentials are invalid or expired.
    InvalidCredentials { reason: String },
    /// The authenticated user is inactive.
    UserInactive { user_id: String },
    /// An internal error occurred during authentication.
    Internal { message: String },
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCredentials => {
                write!(f, "no authentication credentials provided")
            }
            Self::InvalidCredentials { reason } => {
                write!(f, "invalid credentials: {reason}")
            }
            Self::UserInactive { user_id } => {
                write!(f, "user '{user_id}' is inactive")
            }
            Self::Internal { message } => {
                write!(f, "authentication error: {message}")
            }
        }
    }
}

impl std::error::Error for AuthError {}

// ---------------------------------------------------------------------------
// Record-level access control
// ---------------------------------------------------------------------------

/// Trait for record-level access control.
///
/// Implementations decide whether the authenticated user can see, modify, or
/// delete individual records, typically based on the `@owner` field annotation.
pub trait RecordAccessPolicy: Send + Sync {
    /// Filter a list of entities to only those visible to the authenticated user.
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        auth: &'a AuthContext,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>>;

    /// Check if the user can modify (update) the given entity.
    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        auth: &'a AuthContext,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// Check if the user can delete the given entity.
    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        auth: &'a AuthContext,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// Default record-level access policy based on the `@owner` field annotation.
///
/// If the schema has a field annotated with `@owner`, only the entity owner
/// (the user whose `EntityId` matches the `@owner` field value) can modify or
/// delete it. Users with the `"admin"` role bypass ownership checks.
/// Schemas without `@owner` have no record-level restrictions.
pub struct OwnershipBasedPolicy;

impl RecordAccessPolicy for OwnershipBasedPolicy {
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        auth: &'a AuthContext,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>> {
        Box::pin(async move {
            if auth.is_admin() {
                return entities;
            }
            let owner_field = match find_owner_field(schema) {
                Some(name) => name,
                None => return entities,
            };
            entities
                .into_iter()
                .filter(|e| {
                    e.fields
                        .get(&owner_field)
                        .is_some_and(|val| matches_user_id(val, &auth.user_id))
                })
                .collect()
        })
    }

    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        auth: &'a AuthContext,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { is_owner_or_admin(schema, auth, entity) })
    }

    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        auth: &'a AuthContext,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { is_owner_or_admin(schema, auth, entity) })
    }
}

/// Find the field name annotated with `@owner`, if any.
fn find_owner_field(schema: &SchemaDefinition) -> Option<String> {
    schema.fields.iter().find_map(|f| {
        if f.annotations
            .iter()
            .any(|a| matches!(a, FieldAnnotation::Owner))
        {
            Some(f.name.as_str().to_string())
        } else {
            None
        }
    })
}

/// Compare a `DynamicValue` against a user `EntityId`.
///
/// The `@owner` field stores the owner's entity ID as `DynamicValue::Text`.
fn matches_user_id(val: &DynamicValue, user_id: &EntityId) -> bool {
    match val {
        DynamicValue::Text(s) => s == user_id.as_str(),
        _ => false,
    }
}

/// Check if the user is the owner of the entity or has the admin role.
///
/// Returns `true` if the user has the admin role, the schema has no `@owner`
/// annotation, or the entity's owner field matches the user's entity ID.
fn is_owner_or_admin(schema: &SchemaDefinition, auth: &AuthContext, entity: &Entity) -> bool {
    if auth.is_admin() {
        return true;
    }
    let owner_field = match find_owner_field(schema) {
        Some(name) => name,
        None => return true,
    };
    match entity.fields.get(&owner_field) {
        Some(val) => matches_user_id(val, &auth.user_id),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(roles: Vec<&str>) -> AuthContext {
        AuthContext {
            user_id: EntityId::new(),
            roles: roles.into_iter().map(String::from).collect(),
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn has_role_returns_true_for_matching_role() {
        let ctx = make_context(vec!["admin", "member"]);
        assert!(ctx.has_role("admin"));
    }

    #[test]
    fn has_role_returns_false_for_missing_role() {
        let ctx = make_context(vec!["member"]);
        assert!(!ctx.has_role("admin"));
    }

    #[test]
    fn has_any_role_returns_true_when_one_matches() {
        let ctx = make_context(vec!["member"]);
        assert!(ctx.has_any_role(&["admin".to_string(), "member".to_string()]));
    }

    #[test]
    fn has_any_role_returns_false_when_none_match() {
        let ctx = make_context(vec!["viewer"]);
        assert!(!ctx.has_any_role(&["admin".to_string(), "member".to_string()]));
    }

    #[test]
    fn has_any_role_returns_false_for_empty_input() {
        let ctx = make_context(vec!["admin"]);
        assert!(!ctx.has_any_role(&[]));
    }

    #[test]
    fn is_admin_returns_true_with_admin_role() {
        let ctx = make_context(vec!["admin"]);
        assert!(ctx.is_admin());
    }

    #[test]
    fn is_admin_returns_false_without_admin_role() {
        let ctx = make_context(vec!["member"]);
        assert!(!ctx.is_admin());
    }

    #[test]
    fn auth_error_display_missing_credentials() {
        let err = AuthError::MissingCredentials;
        assert!(err.to_string().contains("no authentication credentials"));
    }

    #[test]
    fn auth_error_display_invalid_credentials() {
        let err = AuthError::InvalidCredentials {
            reason: "token expired".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("token expired"));
    }

    #[test]
    fn auth_error_display_user_inactive() {
        let err = AuthError::UserInactive {
            user_id: "user_123".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("user_123"));
    }

    #[test]
    fn auth_error_display_internal() {
        let err = AuthError::Internal {
            message: "db timeout".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("db timeout"));
    }

    #[test]
    fn auth_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(AuthError::MissingCredentials);
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn auth_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthError>();
    }

    #[test]
    fn auth_context_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthContext>();
    }

    // -- RecordAccessPolicy / OwnershipBasedPolicy tests --

    fn make_schema_with_owner() -> SchemaDefinition {
        use schema_forge_core::types::{
            FieldDefinition, FieldName, FieldType, SchemaId, TextConstraints,
        };
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Task").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("title").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("owner_id").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![],
                    vec![FieldAnnotation::Owner],
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    fn make_schema_without_owner() -> SchemaDefinition {
        use schema_forge_core::types::{
            FieldDefinition, FieldName, FieldType, SchemaId, TextConstraints,
        };
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Note").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("body").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    fn make_entity_with_owner(schema_name: &str, owner_id: &str) -> Entity {
        Entity::new(
            SchemaName::new(schema_name).unwrap(),
            BTreeMap::from([
                ("title".to_string(), DynamicValue::Text("My Task".into())),
                (
                    "owner_id".to_string(),
                    DynamicValue::Text(owner_id.to_string()),
                ),
            ]),
        )
    }

    #[tokio::test]
    async fn filter_visible_returns_all_for_admin() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let admin = make_context(vec!["admin"]);
        let other_id = EntityId::new();

        let entities = vec![
            make_entity_with_owner("Task", other_id.as_str()),
            make_entity_with_owner("Task", "someone_else"),
        ];

        let result = policy.filter_visible(&schema, &admin, entities).await;
        assert_eq!(result.len(), 2);
    }

    #[tokio::test]
    async fn filter_visible_filters_by_owner() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let user_id = EntityId::new();
        let user = AuthContext {
            user_id: user_id.clone(),
            roles: vec!["member".to_string()],
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        };

        let entities = vec![
            make_entity_with_owner("Task", user_id.as_str()),
            make_entity_with_owner("Task", "someone_else"),
        ];

        let result = policy.filter_visible(&schema, &user, entities).await;
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].fields.get("owner_id"),
            Some(&DynamicValue::Text(user_id.as_str().to_string()))
        );
    }

    #[tokio::test]
    async fn filter_visible_returns_all_when_no_owner_field() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_without_owner();
        let user = make_context(vec!["member"]);

        let entities = vec![Entity::new(
            SchemaName::new("Note").unwrap(),
            BTreeMap::from([("body".to_string(), DynamicValue::Text("hello".into()))]),
        )];

        let result = policy.filter_visible(&schema, &user, entities).await;
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn can_modify_allows_owner() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let user_id = EntityId::new();
        let user = AuthContext {
            user_id: user_id.clone(),
            roles: vec!["member".to_string()],
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        };
        let entity = make_entity_with_owner("Task", user_id.as_str());

        assert!(policy.can_modify(&schema, &user, &entity).await);
    }

    #[tokio::test]
    async fn can_modify_rejects_non_owner() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let user = AuthContext {
            user_id: EntityId::new(),
            roles: vec!["member".to_string()],
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        };
        let entity = make_entity_with_owner("Task", "someone_else");

        assert!(!policy.can_modify(&schema, &user, &entity).await);
    }

    #[tokio::test]
    async fn can_modify_allows_when_no_owner_annotation() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_without_owner();
        let user = make_context(vec!["member"]);
        let entity = Entity::new(
            SchemaName::new("Note").unwrap(),
            BTreeMap::from([("body".to_string(), DynamicValue::Text("hello".into()))]),
        );

        assert!(policy.can_modify(&schema, &user, &entity).await);
    }

    #[tokio::test]
    async fn can_delete_allows_admin() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let admin = make_context(vec!["admin"]);
        let entity = make_entity_with_owner("Task", "someone_else");

        assert!(policy.can_delete(&schema, &admin, &entity).await);
    }

    #[tokio::test]
    async fn can_delete_denies_when_owner_field_missing_on_entity() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let user = make_context(vec!["member"]);
        // Entity without the owner_id field
        let entity = Entity::new(
            SchemaName::new("Task").unwrap(),
            BTreeMap::from([("title".to_string(), DynamicValue::Text("No owner".into()))]),
        );

        assert!(!policy.can_delete(&schema, &user, &entity).await);
    }

    #[test]
    fn record_access_policy_is_object_safe() {
        fn assert_object_safe(_: &dyn RecordAccessPolicy) {}
        let policy = OwnershipBasedPolicy;
        assert_object_safe(&policy);
    }
}
