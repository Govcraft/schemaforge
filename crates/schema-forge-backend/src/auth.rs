use std::future::Future;
use std::pin::Pin;

use acton_service::middleware::Claims;
use schema_forge_core::types::{DynamicValue, FieldAnnotation, SchemaDefinition};

use crate::entity::Entity;

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Extract the user entity ID from a Claims subject.
///
/// Supports both plain IDs ("entity_abc123") and prefixed ("user:entity_abc123").
fn user_id_from_sub(sub: &str) -> &str {
    sub.strip_prefix("user:").unwrap_or(sub)
}

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
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>>;

    /// Check if the user can modify (update) the given entity.
    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// Check if the user can delete the given entity.
    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// Default record-level access policy based on the `@owner` field annotation.
///
/// If the schema has a field annotated with `@owner`, only the entity owner
/// (the user whose ID matches the `@owner` field value) can modify or
/// delete it. Users with the `"admin"` role bypass ownership checks.
/// Schemas without `@owner` have no record-level restrictions.
pub struct OwnershipBasedPolicy;

impl RecordAccessPolicy for OwnershipBasedPolicy {
    fn filter_visible<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entities: Vec<Entity>,
    ) -> Pin<Box<dyn Future<Output = Vec<Entity>> + Send + 'a>> {
        Box::pin(async move {
            if claims.has_role("admin") {
                return entities;
            }
            let owner_field = match find_owner_field(schema) {
                Some(name) => name,
                None => return entities,
            };
            let user_id = user_id_from_sub(&claims.sub);
            entities
                .into_iter()
                .filter(|e| {
                    e.fields
                        .get(&owner_field)
                        .is_some_and(|val| matches!(val, DynamicValue::Text(s) if s == user_id))
                })
                .collect()
        })
    }

    fn can_modify<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { is_owner_or_admin(schema, claims, entity) })
    }

    fn can_delete<'a>(
        &'a self,
        schema: &'a SchemaDefinition,
        claims: &'a Claims,
        entity: &'a Entity,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { is_owner_or_admin(schema, claims, entity) })
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

/// Check if the user is the owner of the entity or has the admin role.
///
/// Returns `true` if the user has the admin role, the schema has no `@owner`
/// annotation, or the entity's owner field matches the user's ID.
fn is_owner_or_admin(schema: &SchemaDefinition, claims: &Claims, entity: &Entity) -> bool {
    if claims.has_role("admin") {
        return true;
    }
    let owner_field = match find_owner_field(schema) {
        Some(name) => name,
        None => return true,
    };
    let user_id = user_id_from_sub(&claims.sub);
    match entity.fields.get(&owner_field) {
        Some(val) => matches!(val, DynamicValue::Text(s) if s == user_id),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use schema_forge_core::types::{
        DynamicValue, EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType,
        SchemaDefinition, SchemaId, SchemaName, TextConstraints,
    };

    use super::*;

    fn make_claims(roles: &[&str]) -> Claims {
        Claims {
            sub: format!("user:{}", EntityId::new().as_str()),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            perms: vec![],
            exp: 9_999_999_999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            email: None,
            username: None,
            custom: HashMap::new(),
        }
    }

    fn make_claims_with_sub(sub: &str, roles: &[&str]) -> Claims {
        Claims {
            sub: sub.to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            perms: vec![],
            exp: 9_999_999_999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            email: None,
            username: None,
            custom: HashMap::new(),
        }
    }

    fn make_schema_with_owner() -> SchemaDefinition {
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

    // -- user_id_from_sub tests --

    #[test]
    fn user_id_from_sub_strips_prefix() {
        assert_eq!(user_id_from_sub("user:entity_abc123"), "entity_abc123");
    }

    #[test]
    fn user_id_from_sub_passthrough_plain() {
        assert_eq!(user_id_from_sub("entity_abc123"), "entity_abc123");
    }

    // -- RecordAccessPolicy / OwnershipBasedPolicy tests --

    #[tokio::test]
    async fn filter_visible_returns_all_for_admin() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let admin = make_claims(&["admin"]);
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
        let claims = make_claims_with_sub(&format!("user:{}", user_id.as_str()), &["member"]);

        let entities = vec![
            make_entity_with_owner("Task", user_id.as_str()),
            make_entity_with_owner("Task", "someone_else"),
        ];

        let result = policy.filter_visible(&schema, &claims, entities).await;
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
        let claims = make_claims(&["member"]);

        let entities = vec![Entity::new(
            SchemaName::new("Note").unwrap(),
            BTreeMap::from([("body".to_string(), DynamicValue::Text("hello".into()))]),
        )];

        let result = policy.filter_visible(&schema, &claims, entities).await;
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn can_modify_allows_owner() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let user_id = EntityId::new();
        let claims = make_claims_with_sub(&format!("user:{}", user_id.as_str()), &["member"]);
        let entity = make_entity_with_owner("Task", user_id.as_str());

        assert!(policy.can_modify(&schema, &claims, &entity).await);
    }

    #[tokio::test]
    async fn can_modify_rejects_non_owner() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let claims = make_claims(&["member"]);
        let entity = make_entity_with_owner("Task", "someone_else");

        assert!(!policy.can_modify(&schema, &claims, &entity).await);
    }

    #[tokio::test]
    async fn can_modify_allows_when_no_owner_annotation() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_without_owner();
        let claims = make_claims(&["member"]);
        let entity = Entity::new(
            SchemaName::new("Note").unwrap(),
            BTreeMap::from([("body".to_string(), DynamicValue::Text("hello".into()))]),
        );

        assert!(policy.can_modify(&schema, &claims, &entity).await);
    }

    #[tokio::test]
    async fn can_delete_allows_admin() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let admin = make_claims(&["admin"]);
        let entity = make_entity_with_owner("Task", "someone_else");

        assert!(policy.can_delete(&schema, &admin, &entity).await);
    }

    #[tokio::test]
    async fn can_delete_denies_when_owner_field_missing_on_entity() {
        let policy = OwnershipBasedPolicy;
        let schema = make_schema_with_owner();
        let claims = make_claims(&["member"]);
        // Entity without the owner_id field
        let entity = Entity::new(
            SchemaName::new("Task").unwrap(),
            BTreeMap::from([("title".to_string(), DynamicValue::Text("No owner".into()))]),
        );

        assert!(!policy.can_delete(&schema, &claims, &entity).await);
    }

    #[test]
    fn record_access_policy_is_object_safe() {
        fn assert_object_safe(_: &dyn RecordAccessPolicy) {}
        let policy = OwnershipBasedPolicy;
        assert_object_safe(&policy);
    }
}
