use schema_forge_backend::auth::AuthContext;
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{Annotation, FieldAnnotation, FieldDefinition, SchemaDefinition};

use crate::error::ForgeError;

/// Actions that can be checked against schema-level `@access` annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessAction {
    /// Reading entities (GET single, GET list).
    Read,
    /// Creating or updating entities (POST, PUT).
    Write,
    /// Deleting entities (DELETE).
    Delete,
}

/// Direction for field-level access filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldFilterDirection {
    /// Filter fields for read operations (GET responses).
    Read,
    /// Filter fields for write operations (POST/PUT request bodies).
    Write,
}

/// Extractor that optionally extracts `AuthContext` from request extensions.
///
/// Required because axum's `Extension<T>` rejects the request if `T`
/// is not present. Since auth is optional (open access mode), we need
/// a custom extractor that returns `None` when no `AuthContext` exists.
pub struct OptionalAuth(pub Option<AuthContext>);

impl<S> axum::extract::FromRequestParts<S> for OptionalAuth
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(OptionalAuth(parts.extensions.get::<AuthContext>().cloned()))
    }
}

/// Check if the authenticated user has access to perform the given action.
///
/// Access rules (in order):
/// 1. No `AuthContext` (open access mode) => permit
/// 2. User has "admin" role => permit (bypass)
/// 3. Schema has no `@access` annotation => permit all authenticated users
/// 4. `@access` role list for the action is empty => permit all authenticated users
/// 5. User must have at least one role from the action's role list
pub fn check_schema_access(
    schema: &SchemaDefinition,
    auth: Option<&AuthContext>,
    action: AccessAction,
) -> Result<(), ForgeError> {
    // Rule 1: no auth context means open access mode
    let auth = match auth {
        Some(ctx) => ctx,
        None => return Ok(()),
    };

    // Rule 2: admin bypass
    if auth.is_admin() {
        return Ok(());
    }

    // Rule 3: no @access annotation means all authenticated users are permitted
    let (read_roles, write_roles, delete_roles) = match find_access_annotation(schema) {
        Some(roles) => roles,
        None => return Ok(()),
    };

    // Select the role list for the requested action
    let required_roles = match action {
        AccessAction::Read => read_roles,
        AccessAction::Write => write_roles,
        AccessAction::Delete => delete_roles,
    };

    // Rule 4: empty role list means all authenticated users are permitted
    if required_roles.is_empty() {
        return Ok(());
    }

    // Rule 5: user must have at least one matching role
    if auth.has_any_role(required_roles) {
        Ok(())
    } else {
        Err(ForgeError::Forbidden {
            message: format!(
                "access denied: user lacks required role for {:?} on schema '{}'",
                action,
                schema.name.as_str(),
            ),
        })
    }
}

/// Extract `@access` annotation from schema, returns (read, write, delete) role lists.
fn find_access_annotation(
    schema: &SchemaDefinition,
) -> Option<(&Vec<String>, &Vec<String>, &Vec<String>)> {
    schema.access_annotation().and_then(|ann| match ann {
        Annotation::Access {
            read,
            write,
            delete,
            ..
        } => Some((read, write, delete)),
        _ => None,
    })
}

/// Filter entity fields based on `@field_access` annotations.
///
/// Silently removes fields the user cannot access (no error).
///
/// Rules:
/// 1. No `AuthContext` => no filtering (open access)
/// 2. Admin role => no filtering (bypass)
/// 3. No `@field_access` on field => field is accessible
/// 4. Empty role list for direction => field is accessible
/// 5. User must have at least one matching role
pub fn filter_entity_fields(
    entity: &mut Entity,
    schema: &SchemaDefinition,
    auth: Option<&AuthContext>,
    direction: FieldFilterDirection,
) {
    // Rule 1: no auth context means open access mode -- no filtering
    let auth = match auth {
        Some(ctx) => ctx,
        None => return,
    };

    // Rule 2: admin bypass
    if auth.is_admin() {
        return;
    }

    // Collect field names to remove
    let fields_to_remove: Vec<String> = entity
        .fields
        .keys()
        .filter(|field_name| {
            if let Some(field_def) = schema.field(field_name) {
                !is_field_accessible(field_def, &auth.roles, direction)
            } else {
                // Unknown field (not in schema) -- keep it accessible
                false
            }
        })
        .cloned()
        .collect();

    for name in fields_to_remove {
        entity.fields.remove(&name);
    }
}

/// Check if a single field is accessible to the given roles in the given direction.
fn is_field_accessible(
    field: &FieldDefinition,
    user_roles: &[String],
    direction: FieldFilterDirection,
) -> bool {
    // Rule 3: no @field_access annotation means field is accessible
    let annotation = match field.field_access() {
        Some(ann) => ann,
        None => return true,
    };

    let required_roles = match annotation {
        FieldAnnotation::FieldAccess { read, write } => match direction {
            FieldFilterDirection::Read => read,
            FieldFilterDirection::Write => write,
        },
        _ => return true,
    };

    // Rule 4: empty role list means field is accessible
    if required_roles.is_empty() {
        return true;
    }

    // Rule 5: user must have at least one matching role
    required_roles
        .iter()
        .any(|role| user_roles.iter().any(|r| r == role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType, SchemaId, SchemaName,
        TextConstraints,
    };
    use std::collections::BTreeMap;

    use schema_forge_core::types::DynamicValue;

    fn make_auth(roles: &[&str]) -> AuthContext {
        AuthContext {
            user_id: EntityId::new(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            tenant_chain: Vec::new(),
            attributes: BTreeMap::new(),
        }
    }

    fn make_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    fn make_field_with_access(name: &str, read: &[&str], write: &[&str]) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![],
            vec![FieldAnnotation::FieldAccess {
                read: read.iter().map(|r| r.to_string()).collect(),
                write: write.iter().map(|r| r.to_string()).collect(),
            }],
        )
    }

    fn make_open_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Open").unwrap(),
            vec![make_field("name")],
            vec![],
        )
        .unwrap()
    }

    fn make_access_schema(read: &[&str], write: &[&str], delete: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Secured").unwrap(),
            vec![make_field("name")],
            vec![Annotation::Access {
                read: read.iter().map(|r| r.to_string()).collect(),
                write: write.iter().map(|r| r.to_string()).collect(),
                delete: delete.iter().map(|r| r.to_string()).collect(),
                cross_tenant_read: vec![],
            }],
        )
        .unwrap()
    }

    fn make_entity_with_fields(fields: &[(&str, &str)]) -> Entity {
        let field_map: BTreeMap<String, DynamicValue> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), DynamicValue::Text(v.to_string())))
            .collect();
        Entity::new(SchemaName::new("Test").unwrap(), field_map)
    }

    // -----------------------------------------------------------------------
    // check_schema_access tests
    // -----------------------------------------------------------------------

    #[test]
    fn check_schema_access_permits_when_no_auth() {
        let schema = make_access_schema(&["viewer"], &["editor"], &["admin"]);
        let result = check_schema_access(&schema, None, AccessAction::Write);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_permits_when_no_access_annotation() {
        let schema = make_open_schema();
        let auth = make_auth(&["member"]);
        let result = check_schema_access(&schema, Some(&auth), AccessAction::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_permits_admin_always() {
        let schema = make_access_schema(&["viewer"], &["editor"], &["superadmin"]);
        let auth = make_auth(&["admin"]);
        let result = check_schema_access(&schema, Some(&auth), AccessAction::Delete);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_permits_matching_role() {
        let schema = make_access_schema(&["viewer", "editor"], &["editor"], &["admin"]);
        let auth = make_auth(&["editor"]);
        let result = check_schema_access(&schema, Some(&auth), AccessAction::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_rejects_non_matching_role() {
        let schema = make_access_schema(&["viewer"], &["editor"], &["admin"]);
        let auth = make_auth(&["guest"]);
        let result = check_schema_access(&schema, Some(&auth), AccessAction::Write);
        assert!(result.is_err());
        assert!(matches!(result, Err(ForgeError::Forbidden { .. })));
    }

    #[test]
    fn check_schema_access_permits_when_role_list_empty() {
        let schema = make_access_schema(&[], &["editor"], &["admin"]);
        let auth = make_auth(&["guest"]);
        // Empty read list means all authenticated users are permitted
        let result = check_schema_access(&schema, Some(&auth), AccessAction::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_read_write_delete_independent() {
        let schema = make_access_schema(&["reader"], &["writer"], &["deleter"]);

        let reader = make_auth(&["reader"]);
        assert!(check_schema_access(&schema, Some(&reader), AccessAction::Read).is_ok());
        assert!(check_schema_access(&schema, Some(&reader), AccessAction::Write).is_err());
        assert!(check_schema_access(&schema, Some(&reader), AccessAction::Delete).is_err());

        let writer = make_auth(&["writer"]);
        assert!(check_schema_access(&schema, Some(&writer), AccessAction::Read).is_err());
        assert!(check_schema_access(&schema, Some(&writer), AccessAction::Write).is_ok());
        assert!(check_schema_access(&schema, Some(&writer), AccessAction::Delete).is_err());

        let deleter = make_auth(&["deleter"]);
        assert!(check_schema_access(&schema, Some(&deleter), AccessAction::Read).is_err());
        assert!(check_schema_access(&schema, Some(&deleter), AccessAction::Write).is_err());
        assert!(check_schema_access(&schema, Some(&deleter), AccessAction::Delete).is_ok());
    }

    // -----------------------------------------------------------------------
    // filter_entity_fields tests
    // -----------------------------------------------------------------------

    #[test]
    fn filter_entity_fields_strips_restricted_on_read() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                make_field("name"),
                make_field_with_access("salary", &["hr"], &["hr"]),
            ],
            vec![],
        )
        .unwrap();

        let auth = make_auth(&["member"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&auth),
            FieldFilterDirection::Read,
        );

        assert!(entity.fields.contains_key("name"));
        assert!(!entity.fields.contains_key("salary"));
    }

    #[test]
    fn filter_entity_fields_strips_restricted_on_write() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                make_field("name"),
                make_field_with_access("salary", &["hr"], &["hr"]),
            ],
            vec![],
        )
        .unwrap();

        let auth = make_auth(&["member"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&auth),
            FieldFilterDirection::Write,
        );

        assert!(entity.fields.contains_key("name"));
        assert!(!entity.fields.contains_key("salary"));
    }

    #[test]
    fn filter_entity_fields_leaves_unrestricted() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![make_field("name"), make_field("email")],
            vec![],
        )
        .unwrap();

        let auth = make_auth(&["member"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("email", "a@b.com")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&auth),
            FieldFilterDirection::Read,
        );

        assert!(entity.fields.contains_key("name"));
        assert!(entity.fields.contains_key("email"));
    }

    #[test]
    fn filter_entity_fields_admin_bypasses() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                make_field("name"),
                make_field_with_access("salary", &["hr"], &["hr"]),
            ],
            vec![],
        )
        .unwrap();

        let auth = make_auth(&["admin"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&auth),
            FieldFilterDirection::Read,
        );

        assert!(entity.fields.contains_key("name"));
        assert!(entity.fields.contains_key("salary"));
    }

    #[test]
    fn filter_entity_fields_no_auth_no_filtering() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                make_field("name"),
                make_field_with_access("salary", &["hr"], &["hr"]),
            ],
            vec![],
        )
        .unwrap();

        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(&mut entity, &schema, None, FieldFilterDirection::Read);

        assert!(entity.fields.contains_key("name"));
        assert!(entity.fields.contains_key("salary"));
    }

    // -----------------------------------------------------------------------
    // is_field_accessible tests
    // -----------------------------------------------------------------------

    #[test]
    fn is_field_accessible_no_annotation() {
        let field = make_field("name");
        assert!(is_field_accessible(
            &field,
            &["member".to_string()],
            FieldFilterDirection::Read
        ));
    }

    #[test]
    fn is_field_accessible_empty_roles() {
        let field = make_field_with_access("salary", &[], &[]);
        assert!(is_field_accessible(
            &field,
            &["member".to_string()],
            FieldFilterDirection::Read
        ));
    }

    #[test]
    fn is_field_accessible_matching_role() {
        let field = make_field_with_access("salary", &["hr"], &["hr"]);
        assert!(is_field_accessible(
            &field,
            &["hr".to_string()],
            FieldFilterDirection::Read
        ));
    }

    #[test]
    fn is_field_accessible_non_matching_role() {
        let field = make_field_with_access("salary", &["hr"], &["hr"]);
        assert!(!is_field_accessible(
            &field,
            &["member".to_string()],
            FieldFilterDirection::Read
        ));
    }

    // -----------------------------------------------------------------------
    // OptionalAuth extractor tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn optional_auth_extracts_when_present() {
        use axum::extract::FromRequestParts;

        let auth = make_auth(&["member"]);
        let (mut parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();
        parts.extensions.insert(auth.clone());

        let result = OptionalAuth::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let OptionalAuth(extracted) = result.unwrap();
        assert!(extracted.is_some());
        assert_eq!(extracted.unwrap().roles, auth.roles);
    }

    #[tokio::test]
    async fn optional_auth_returns_none_when_missing() {
        use axum::extract::FromRequestParts;

        let (mut parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();

        let result = OptionalAuth::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let OptionalAuth(extracted) = result.unwrap();
        assert!(extracted.is_none());
    }
}
