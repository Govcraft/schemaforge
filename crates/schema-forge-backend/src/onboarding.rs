//! Pure validation shared by atomic creator-membership adapters.
use crate::{BackendError, Entity};
use schema_forge_core::types::{DynamicValue, EntityId};

/// Validate the membership's root link and return its existing User reference.
/// The adapter must check User existence inside its write transaction.
pub fn validate_creator_membership<'a>(
    entity: &Entity,
    membership: &'a Entity,
) -> Result<&'a EntityId, BackendError> {
    let invalid = |field: &str, reason: &str| BackendError::ValidationFailed {
        field: field.into(),
        reason: reason.into(),
    };
    if membership.schema.as_str() != "TenantMembership" {
        return Err(invalid(
            "schema",
            "creator membership must use TenantMembership",
        ));
    }
    if !matches!(membership.field("tenant_type"), Some(DynamicValue::Text(value)) if value == entity.schema.as_str())
    {
        return Err(invalid(
            "tenant_type",
            "creator membership must reference the created root schema",
        ));
    }
    if !matches!(membership.field("tenant_id"), Some(DynamicValue::Text(value)) if value == entity.id.as_str())
    {
        return Err(invalid(
            "tenant_id",
            "creator membership must reference the created root ID",
        ));
    }
    match membership.field("user") {
        Some(DynamicValue::Ref(id)) => Ok(id),
        _ => Err(invalid(
            "user",
            "creator membership requires a User reference",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::SchemaName;
    use std::collections::BTreeMap;

    #[test]
    fn membership_cannot_link_another_root_or_non_user() {
        let entity = Entity::new(SchemaName::new("Organization").unwrap(), BTreeMap::new());
        let user = EntityId::new("user");
        let mut membership = Entity::new(
            SchemaName::new("TenantMembership").unwrap(),
            BTreeMap::from([
                ("user".into(), DynamicValue::Ref(user.clone())),
                (
                    "tenant_type".into(),
                    DynamicValue::Text(entity.schema.to_string()),
                ),
                (
                    "tenant_id".into(),
                    DynamicValue::Text(entity.id.to_string()),
                ),
            ]),
        );
        assert_eq!(
            validate_creator_membership(&entity, &membership).unwrap(),
            &user
        );
        for (field, value) in [
            (
                "tenant_id",
                DynamicValue::Text(EntityId::new("organization").to_string()),
            ),
            ("tenant_type", DynamicValue::Text("OtherRoot".into())),
            ("user", DynamicValue::Text(user.to_string())),
        ] {
            let previous = membership.fields.insert(field.into(), value).unwrap();
            assert!(validate_creator_membership(&entity, &membership).is_err());
            membership.fields.insert(field.into(), previous);
        }
        membership.schema = SchemaName::new("OtherMembership").unwrap();
        assert!(validate_creator_membership(&entity, &membership).is_err());
    }
}
