use schema_forge_core::types::SchemaDefinition;

/// A generated Cedar policy template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CedarPolicy {
    /// Human-readable description of what this policy allows.
    pub description: String,
    /// The Cedar policy text.
    pub cedar_text: String,
}

/// Generate Cedar policy templates for a schema.
///
/// Pure function: takes a schema definition, returns policy templates.
/// These are default templates -- administrators customize in `policies/custom/`.
///
/// Generated policies:
/// 1. Read access for any authenticated user
/// 2. Create/update for the entity owner
/// 3. Delete for admin group only
/// 4. Schema modification for schema-admin group only
pub fn generate_cedar_policies(schema: &SchemaDefinition) -> Vec<CedarPolicy> {
    let name = schema.name.as_str();
    vec![
        read_policy(name),
        owner_write_policy(name),
        admin_delete_policy(name),
        schema_admin_policy(name),
    ]
}

/// Render a single Cedar policy for read access.
///
/// Any authenticated user can read entities of this schema.
fn read_policy(schema_name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!(
            "Allow any authenticated user to read {schema_name} entities"
        ),
        cedar_text: format!(
            r#"permit (
    principal,
    action == Action::"Read{schema_name}",
    resource is {schema_name}
) when {{
    principal is User
}};"#
        ),
    }
}

/// Render a single Cedar policy for owner write access.
///
/// Only the entity owner (resource.created_by == principal.id) can create/update.
fn owner_write_policy(schema_name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!(
            "Allow the entity owner to create and update {schema_name} entities"
        ),
        cedar_text: format!(
            r#"permit (
    principal,
    action in [Action::"Create{schema_name}", Action::"Update{schema_name}"],
    resource is {schema_name}
) when {{
    principal is User &&
    resource.created_by == principal.id
}};"#
        ),
    }
}

/// Render a single Cedar policy for admin delete access.
///
/// Only members of Group::"admin" can delete entities.
fn admin_delete_policy(schema_name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!(
            "Allow admin group members to delete {schema_name} entities"
        ),
        cedar_text: format!(
            r#"permit (
    principal,
    action == Action::"Delete{schema_name}",
    resource is {schema_name}
) when {{
    principal is User &&
    principal in Group::"admin"
}};"#
        ),
    }
}

/// Render a single Cedar policy for schema admin modification.
///
/// Only members of Group::"schema-admin" can modify the schema itself.
fn schema_admin_policy(schema_name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!(
            "Allow schema-admin group members to modify the {schema_name} schema"
        ),
        cedar_text: format!(
            r#"permit (
    principal,
    action in [Action::"UpdateSchema", Action::"DeleteSchema"],
    resource == Schema::"{schema_name}"
) when {{
    principal is User &&
    principal in Group::"schema-admin"
}};"#
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        FieldDefinition, FieldName, FieldType, SchemaId, SchemaName, TextConstraints,
    };

    fn make_test_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn generates_four_policies() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        assert_eq!(policies.len(), 4);
    }

    #[test]
    fn each_policy_contains_schema_name() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        for policy in &policies {
            assert!(
                policy.cedar_text.contains("Contact"),
                "Policy missing schema name: {}",
                policy.description
            );
        }
    }

    #[test]
    fn read_policy_allows_any_authenticated_user() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        let read = &policies[0];
        assert!(read.cedar_text.contains("ReadContact"));
        assert!(read.cedar_text.contains("principal is User"));
        assert!(read.description.contains("read"));
    }

    #[test]
    fn owner_write_policy_checks_created_by() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        let write = &policies[1];
        assert!(write.cedar_text.contains("resource.created_by == principal.id"));
        assert!(write.cedar_text.contains("CreateContact"));
        assert!(write.cedar_text.contains("UpdateContact"));
    }

    #[test]
    fn admin_delete_policy_requires_admin_group() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        let delete = &policies[2];
        assert!(delete.cedar_text.contains("DeleteContact"));
        assert!(delete.cedar_text.contains(r#"Group::"admin""#));
    }

    #[test]
    fn schema_admin_policy_requires_schema_admin_group() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        let admin = &policies[3];
        assert!(admin.cedar_text.contains("UpdateSchema"));
        assert!(admin.cedar_text.contains("DeleteSchema"));
        assert!(admin.cedar_text.contains(r#"Group::"schema-admin""#));
        assert!(admin.cedar_text.contains(r#"Schema::"Contact""#));
    }

    #[test]
    fn pure_function_deterministic() {
        let schema = make_test_schema();
        let first = generate_cedar_policies(&schema);
        let second = generate_cedar_policies(&schema);
        assert_eq!(first, second);
    }

    #[test]
    fn different_schema_produces_different_policies() {
        let contact = make_test_schema();
        let company = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Company").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();

        let contact_policies = generate_cedar_policies(&contact);
        let company_policies = generate_cedar_policies(&company);

        assert!(contact_policies[0].cedar_text.contains("Contact"));
        assert!(company_policies[0].cedar_text.contains("Company"));
        assert_ne!(contact_policies, company_policies);
    }
}
