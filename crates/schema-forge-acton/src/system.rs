use schema_forge_core::migration::DiffEngine;
use schema_forge_core::system_schemas;
use schema_forge_core::types::DynamicValue;

use crate::error::ForgeError;
use crate::state::{DynForgeBackend, SchemaRegistry};

/// Seed system schemas (User, Role, Permission, TenantMembership) into the database.
///
/// This is idempotent -- existing schemas are skipped. Called during extension startup.
pub async fn seed_system_schemas(
    registry: &SchemaRegistry,
    backend: &dyn DynForgeBackend,
) -> Result<(), ForgeError> {
    let dsl_texts = system_schemas::all_system_schemas();

    for dsl_text in dsl_texts {
        let definitions =
            schema_forge_dsl::parse(dsl_text).map_err(|errors| ForgeError::Internal {
                message: format!("Failed to parse system schema: {:?}", errors),
            })?;

        for definition in definitions {
            let name = definition.name.clone();
            let name_str = name.as_str().to_string();

            // Check if schema already exists in backend
            let existing = backend
                .load_schema_metadata(&name)
                .await
                .map_err(ForgeError::from)?;

            if let Some(existing_def) = existing {
                // Schema already exists -- insert into registry cache and skip
                registry.insert(name_str, existing_def).await;
                continue;
            }

            // Generate migration plan for new schema
            let plan = DiffEngine::create_new(&definition);

            // Apply migration
            backend
                .apply_migration(&name, &plan.steps)
                .await
                .map_err(ForgeError::from)?;

            // Store schema metadata
            backend
                .store_schema_metadata(&definition)
                .await
                .map_err(ForgeError::from)?;

            // Insert into registry cache
            registry.insert(name_str, definition).await;
        }
    }

    // Seed default roles
    seed_default_roles(registry, backend).await?;

    Ok(())
}

/// Seed default roles (admin, member, readonly) as Role entities.
async fn seed_default_roles(
    registry: &SchemaRegistry,
    backend: &dyn DynForgeBackend,
) -> Result<(), ForgeError> {
    use schema_forge_backend::entity::Entity;
    use schema_forge_core::types::SchemaName;
    use std::collections::BTreeMap;

    let role_schema_name = SchemaName::new("Role").map_err(|_| ForgeError::Internal {
        message: "Invalid Role schema name".to_string(),
    })?;

    // Get the Role schema definition to obtain its SchemaId for queries
    let role_def = registry
        .get("Role")
        .await
        .ok_or_else(|| ForgeError::Internal {
            message: "Role schema not found in registry after seeding".to_string(),
        })?;

    let query = schema_forge_core::query::Query::new(role_def.id.clone());
    let existing = backend.query(&query).await.map_err(ForgeError::from)?;

    if !existing.entities.is_empty() {
        return Ok(()); // Roles already seeded
    }

    let default_roles = [
        ("admin", "Full access to all operations"),
        ("member", "Standard user with read/write access"),
        ("readonly", "Read-only access to all schemas"),
    ];

    for (name, description) in &default_roles {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text((*name).to_string()));
        fields.insert(
            "description".to_string(),
            DynamicValue::Text((*description).to_string()),
        );

        let entity = Entity::new(role_schema_name.clone(), fields);
        backend.create(&entity).await.map_err(ForgeError::from)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use schema_forge_core::system_schemas;
    use schema_forge_core::types::Annotation;

    #[test]
    fn all_system_schemas_parse_successfully() {
        for dsl_text in system_schemas::all_system_schemas() {
            let result = schema_forge_dsl::parse(dsl_text);
            assert!(
                result.is_ok(),
                "Failed to parse system schema:\n{dsl_text}\nErrors: {:?}",
                result.unwrap_err()
            );
        }
    }

    #[test]
    fn system_schemas_have_system_annotation() {
        for dsl_text in system_schemas::all_system_schemas() {
            let schemas = schema_forge_dsl::parse(dsl_text).unwrap();
            for schema in &schemas {
                assert!(
                    schema
                        .annotations
                        .iter()
                        .any(|a| matches!(a, Annotation::System)),
                    "Schema '{}' missing @system annotation",
                    schema.name
                );
            }
        }
    }

    #[test]
    fn user_schema_has_display_annotation() {
        let schemas = schema_forge_dsl::parse(system_schemas::USER_SCHEMA).unwrap();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name.as_str(), "User");
        assert!(schemas[0]
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::Display { .. })));
    }

    #[test]
    fn role_schema_has_display_annotation() {
        let schemas = schema_forge_dsl::parse(system_schemas::ROLE_SCHEMA).unwrap();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name.as_str(), "Role");
        assert!(schemas[0]
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::Display { .. })));
    }

    #[test]
    fn permission_schema_has_display_annotation() {
        let schemas = schema_forge_dsl::parse(system_schemas::PERMISSION_SCHEMA).unwrap();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name.as_str(), "Permission");
        assert!(schemas[0]
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::Display { .. })));
    }

    #[test]
    fn all_system_schemas_returns_five() {
        assert_eq!(system_schemas::all_system_schemas().len(), 5);
    }

    #[test]
    fn dependency_order_permission_before_role_before_user() {
        let schemas = system_schemas::all_system_schemas();
        let parsed: Vec<String> = schemas
            .iter()
            .flat_map(|dsl| {
                schema_forge_dsl::parse(dsl)
                    .unwrap()
                    .into_iter()
                    .map(|s| s.name.as_str().to_string())
            })
            .collect();
        assert_eq!(parsed[0], "Permission");
        assert_eq!(parsed[1], "Role");
        assert_eq!(parsed[2], "User");
        assert_eq!(parsed[3], "TenantMembership");
        assert_eq!(parsed[4], "Theme");
    }
}
