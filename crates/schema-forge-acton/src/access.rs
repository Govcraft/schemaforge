use std::collections::BTreeMap;

use acton_service::middleware::Claims;
use schema_forge_backend::entity::Entity;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_backend::TenantRef;
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{
    Annotation, DynamicValue, FieldAnnotation, FieldDefinition, SchemaDefinition,
};

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

/// Extractor that optionally extracts `Claims` from request extensions.
///
/// Required because axum's `Extension<T>` rejects the request if `T`
/// is not present. Since claims may not be present (e.g., unauthenticated
/// requests), we need a custom extractor that returns `None` when no
/// `Claims` exists.
pub struct OptionalClaims(pub Option<Claims>);

impl<S> axum::extract::FromRequestParts<S> for OptionalClaims
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(OptionalClaims(parts.extensions.get::<Claims>().cloned()))
    }
}

/// The reserved role name that grants unauthenticated (public) access.
///
/// When a schema's `@access` annotation includes `"public"` in a role list,
/// that action is accessible without any `Claims`.
pub const PUBLIC_ROLE: &str = "public";

/// Check if the authenticated user has access to perform the given action.
///
/// Access rules (secure by default):
/// 1. No `Claims` => **DENY** (authentication required)
/// 2. User has "admin" role => permit (bypass)
/// 3. Schema has no `@access` annotation => **DENY** (secure by default)
/// 4. Role list for the action contains "public" => permit (even without auth)
/// 5. Empty role list for the action => permit all authenticated users
/// 6. User must have at least one role from the action's role list
pub fn check_schema_access(
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    action: AccessAction,
) -> Result<(), ForgeError> {
    // Rule 1: no claims means authentication is required
    let claims = match claims {
        Some(c) => c,
        None => {
            return Err(ForgeError::Unauthorized {
                message: "authentication required".to_string(),
            })
        }
    };

    // Rule 2: admin bypass
    if claims.has_role("admin") {
        return Ok(());
    }

    // Rule 3: no @access annotation = deny (secure by default)
    let (read_roles, write_roles, delete_roles) = match find_access_annotation(schema) {
        Some(roles) => roles,
        None => {
            return Err(ForgeError::Forbidden {
                message: format!(
                    "access denied: schema '{}' has no @access annotation (secure by default)",
                    schema.name.as_str(),
                ),
            })
        }
    };

    // Select the role list for the requested action
    let required_roles = match action {
        AccessAction::Read => read_roles,
        AccessAction::Write => write_roles,
        AccessAction::Delete => delete_roles,
    };

    // Rule 4: "public" in role list = permit all (including unauthenticated once
    // the middleware supports soft-auth pass-through)
    if required_roles.iter().any(|r| r == PUBLIC_ROLE) {
        return Ok(());
    }

    // Rule 5: empty role list means all authenticated users are permitted
    if required_roles.is_empty() {
        return Ok(());
    }

    // Rule 6: user must have at least one matching role
    if required_roles.iter().any(|r| claims.has_role(r)) {
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
/// 1. No `Claims` => no filtering (unauthenticated requests are rejected at
///    the schema level; if they reach here, permit all fields)
/// 2. Admin role => no filtering (bypass)
/// 3. No `@field_access` on field => field is accessible
/// 4. Empty role list for direction => field is accessible
/// 5. User must have at least one matching role
pub fn filter_entity_fields(
    entity: &mut Entity,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    direction: FieldFilterDirection,
) {
    // Rule 1: no claims means open access mode -- no filtering
    let claims = match claims {
        Some(c) => c,
        None => return,
    };

    // Rule 2: admin bypass
    if claims.has_role("admin") {
        return;
    }

    // Collect field names to remove
    let fields_to_remove: Vec<String> = entity
        .fields
        .keys()
        .filter(|field_name| {
            if let Some(field_def) = schema.field(field_name) {
                !is_field_accessible(field_def, &claims.roles, direction)
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

/// Inject tenant scoping filter into a query.
///
/// Adds `_tenant = <tenant_id>` filter based on the deepest tenant in the
/// claims' `tenant_chain` custom claim. No-ops when:
/// - `tenant_config` is `None` or disabled
/// - `claims` is `None`
/// - user is admin (bypass)
pub fn inject_tenant_scope(
    query: &mut Query,
    claims: Option<&Claims>,
    tenant_config: &Option<TenantConfig>,
) {
    let _config = match tenant_config {
        Some(c) if c.is_enabled() => c,
        _ => return,
    };
    let claims = match claims {
        Some(c) => c,
        None => return,
    };
    if claims.has_role("admin") {
        return;
    }
    let tenant_chain: Vec<TenantRef> = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();
    if let Some(tenant_ref) = tenant_chain.last() {
        let tenant_filter = Filter::eq(
            FieldPath::single("_tenant"),
            DynamicValue::Text(tenant_ref.entity_id.clone()),
        );
        query.filter = Some(match query.filter.take() {
            Some(existing) => Filter::and(vec![existing, tenant_filter]),
            None => tenant_filter,
        });
    }
}

/// Inject `_tenant` field into entity fields on creation.
///
/// Sets `_tenant` to the deepest tenant entity ID in the claims'
/// `tenant_chain` custom claim. No-ops when tenancy is disabled,
/// claims is `None`, or the tenant chain is empty.
pub fn inject_tenant_on_create(
    fields: &mut BTreeMap<String, DynamicValue>,
    claims: Option<&Claims>,
    tenant_config: &Option<TenantConfig>,
) {
    let _config = match tenant_config {
        Some(c) if c.is_enabled() => c,
        _ => return,
    };
    let claims = match claims {
        Some(c) => c,
        None => return,
    };
    let tenant_chain: Vec<TenantRef> = claims
        .custom_claim_as::<Vec<TenantRef>>("tenant_chain")
        .unwrap_or_default();
    if let Some(tenant_ref) = tenant_chain.last() {
        fields.insert(
            "_tenant".to_string(),
            DynamicValue::Text(tenant_ref.entity_id.clone()),
        );
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
        Annotation, EntityId, FieldAnnotation, FieldDefinition, FieldName, FieldType, SchemaId,
        SchemaName, TenantKind, TextConstraints,
    };
    use std::collections::{BTreeMap, HashMap};

    use schema_forge_core::types::DynamicValue;

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

    fn make_claims_with_tenant(roles: &[&str], tenant_entity_id: &str) -> Claims {
        let mut claims = make_claims(roles);
        claims.custom.insert(
            "tenant_chain".to_string(),
            serde_json::json!([{"schema": "Organization", "entity_id": tenant_entity_id}]),
        );
        claims
    }

    fn make_enabled_tenant_config() -> Option<TenantConfig> {
        let schemas = vec![SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Organization").unwrap(),
            vec![make_field("name")],
            vec![Annotation::Tenant(TenantKind::Root)],
        )
        .unwrap()];
        Some(TenantConfig::from_schemas(&schemas).unwrap())
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
    fn check_schema_access_denies_when_no_claims() {
        // No Claims = authentication required — always denies
        let schema = make_access_schema(&["viewer"], &["editor"], &["admin"]);
        let result = check_schema_access(&schema, None, AccessAction::Write);
        assert!(result.is_err());
        assert!(matches!(result, Err(ForgeError::Unauthorized { .. })));
    }

    #[test]
    fn check_schema_access_denies_when_no_claims_no_annotation() {
        // No Claims = authentication required even without @access annotation
        let schema = make_open_schema();
        let result = check_schema_access(&schema, None, AccessAction::Read);
        assert!(result.is_err());
        assert!(matches!(result, Err(ForgeError::Unauthorized { .. })));
    }

    #[test]
    fn check_schema_access_denies_when_auth_but_no_access_annotation() {
        // Auth configured + no @access = secure by default -> deny
        let schema = make_open_schema();
        let claims = make_claims(&["member"]);
        let result = check_schema_access(&schema, Some(&claims), AccessAction::Read);
        assert!(result.is_err());
        assert!(matches!(result, Err(ForgeError::Forbidden { .. })));
    }

    #[test]
    fn check_schema_access_permits_public_role() {
        // "public" in role list = permit for any authenticated user
        let schema = make_access_schema(&["public"], &["editor"], &["admin"]);
        let claims = make_claims(&["anyone"]);
        let result = check_schema_access(&schema, Some(&claims), AccessAction::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_permits_admin_always() {
        let schema = make_access_schema(&["viewer"], &["editor"], &["superadmin"]);
        let claims = make_claims(&["admin"]);
        let result = check_schema_access(&schema, Some(&claims), AccessAction::Delete);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_permits_matching_role() {
        let schema = make_access_schema(&["viewer", "editor"], &["editor"], &["admin"]);
        let claims = make_claims(&["editor"]);
        let result = check_schema_access(&schema, Some(&claims), AccessAction::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_rejects_non_matching_role() {
        let schema = make_access_schema(&["viewer"], &["editor"], &["admin"]);
        let claims = make_claims(&["guest"]);
        let result = check_schema_access(&schema, Some(&claims), AccessAction::Write);
        assert!(result.is_err());
        assert!(matches!(result, Err(ForgeError::Forbidden { .. })));
    }

    #[test]
    fn check_schema_access_permits_when_role_list_empty() {
        let schema = make_access_schema(&[], &["editor"], &["admin"]);
        let claims = make_claims(&["guest"]);
        // Empty read list means all authenticated users are permitted
        let result = check_schema_access(&schema, Some(&claims), AccessAction::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn check_schema_access_read_write_delete_independent() {
        let schema = make_access_schema(&["reader"], &["writer"], &["deleter"]);

        let reader = make_claims(&["reader"]);
        assert!(check_schema_access(&schema, Some(&reader), AccessAction::Read).is_ok());
        assert!(check_schema_access(&schema, Some(&reader), AccessAction::Write).is_err());
        assert!(check_schema_access(&schema, Some(&reader), AccessAction::Delete).is_err());

        let writer = make_claims(&["writer"]);
        assert!(check_schema_access(&schema, Some(&writer), AccessAction::Read).is_err());
        assert!(check_schema_access(&schema, Some(&writer), AccessAction::Write).is_ok());
        assert!(check_schema_access(&schema, Some(&writer), AccessAction::Delete).is_err());

        let deleter = make_claims(&["deleter"]);
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

        let claims = make_claims(&["member"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&claims),
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

        let claims = make_claims(&["member"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&claims),
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

        let claims = make_claims(&["member"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("email", "a@b.com")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&claims),
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

        let claims = make_claims(&["admin"]);
        let mut entity = make_entity_with_fields(&[("name", "Alice"), ("salary", "100000")]);

        filter_entity_fields(
            &mut entity,
            &schema,
            Some(&claims),
            FieldFilterDirection::Read,
        );

        assert!(entity.fields.contains_key("name"));
        assert!(entity.fields.contains_key("salary"));
    }

    #[test]
    fn filter_entity_fields_no_claims_no_filtering() {
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
    // OptionalClaims extractor tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn optional_claims_extracts_when_present() {
        use axum::extract::FromRequestParts;

        let claims = make_claims(&["member"]);
        let (mut parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();
        parts.extensions.insert(claims.clone());

        let result = OptionalClaims::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let OptionalClaims(extracted) = result.unwrap();
        assert!(extracted.is_some());
        assert_eq!(extracted.unwrap().roles, claims.roles);
    }

    #[tokio::test]
    async fn optional_claims_returns_none_when_missing() {
        use axum::extract::FromRequestParts;

        let (mut parts, _body) = axum::http::Request::builder()
            .uri("/test")
            .body(())
            .unwrap()
            .into_parts();

        let result = OptionalClaims::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let OptionalClaims(extracted) = result.unwrap();
        assert!(extracted.is_none());
    }

    // -----------------------------------------------------------------------
    // inject_tenant_scope tests
    // -----------------------------------------------------------------------

    #[test]
    fn inject_tenant_scope_adds_filter_when_enabled() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new();
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_some());
        let filter = query.filter.unwrap();
        match filter {
            Filter::Eq {
                ref path,
                ref value,
            } => {
                assert_eq!(path.root(), "_tenant");
                assert_eq!(*value, DynamicValue::Text(tenant_id.as_str().to_string()));
            }
            _ => panic!("expected Eq filter, got: {filter:?}"),
        }
    }

    #[test]
    fn inject_tenant_scope_noop_when_disabled() {
        let tenant_config: Option<TenantConfig> = None;
        let tenant_id = EntityId::new();
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_none());
    }

    #[test]
    fn inject_tenant_scope_noop_for_admin() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new();
        let claims = make_claims_with_tenant(&["admin"], tenant_id.as_str());
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_none());
    }

    #[test]
    fn inject_tenant_scope_noop_when_no_claims() {
        let tenant_config = make_enabled_tenant_config();
        let mut query = Query::new(SchemaId::new());

        inject_tenant_scope(&mut query, None, &tenant_config);

        assert!(query.filter.is_none());
    }

    #[test]
    fn inject_tenant_scope_combines_with_existing_filter() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new();
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());

        let existing_filter = Filter::eq(
            FieldPath::single("status"),
            DynamicValue::Text("active".to_string()),
        );
        let mut query = Query::new(SchemaId::new()).with_filter(existing_filter);

        inject_tenant_scope(&mut query, Some(&claims), &tenant_config);

        assert!(query.filter.is_some());
        let filter = query.filter.unwrap();
        match filter {
            Filter::And { ref filters } => {
                assert_eq!(filters.len(), 2);
                // First filter is the original
                match &filters[0] {
                    Filter::Eq { path, .. } => assert_eq!(path.root(), "status"),
                    other => panic!("expected Eq filter, got: {other:?}"),
                }
                // Second filter is the tenant filter
                match &filters[1] {
                    Filter::Eq { path, value } => {
                        assert_eq!(path.root(), "_tenant");
                        assert_eq!(*value, DynamicValue::Text(tenant_id.as_str().to_string()));
                    }
                    other => panic!("expected Eq filter, got: {other:?}"),
                }
            }
            _ => panic!("expected And filter, got: {filter:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // inject_tenant_on_create tests
    // -----------------------------------------------------------------------

    #[test]
    fn inject_tenant_on_create_inserts_tenant_field() {
        let tenant_config = make_enabled_tenant_config();
        let tenant_id = EntityId::new();
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, Some(&claims), &tenant_config);

        assert!(fields.contains_key("_tenant"));
        assert_eq!(
            fields["_tenant"],
            DynamicValue::Text(tenant_id.as_str().to_string())
        );
    }

    #[test]
    fn inject_tenant_on_create_noop_when_disabled() {
        let tenant_config: Option<TenantConfig> = None;
        let tenant_id = EntityId::new();
        let claims = make_claims_with_tenant(&["member"], tenant_id.as_str());
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, Some(&claims), &tenant_config);

        assert!(!fields.contains_key("_tenant"));
    }

    #[test]
    fn inject_tenant_on_create_noop_when_no_claims() {
        let tenant_config = make_enabled_tenant_config();
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, None, &tenant_config);

        assert!(!fields.contains_key("_tenant"));
    }

    #[test]
    fn inject_tenant_on_create_noop_when_empty_tenant_chain() {
        let tenant_config = make_enabled_tenant_config();
        let claims = make_claims(&["member"]); // no tenant chain
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".to_string()));

        inject_tenant_on_create(&mut fields, Some(&claims), &tenant_config);

        assert!(!fields.contains_key("_tenant"));
    }
}
