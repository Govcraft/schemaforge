use std::collections::HashMap;

use schema_forge_core::migration::DiffEngine;
use schema_forge_core::system_schemas;
use schema_forge_core::types::SchemaDefinition;

use crate::error::ForgeError;
use crate::state::{DynForgeBackend, SchemaRegistry};

/// Seed system schemas (User, TenantMembership, WebhookSubscription) into the
/// database.
///
/// Idempotent: existing schemas are reused. No roles or permissions are
/// pre-seeded — Cedar policies are the source of truth for both, and a fresh
/// deployment carries only whatever roles its generated and custom policies
/// reference, plus the built-in `platform_admin` role assigned during
/// first-run bootstrap.
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

            let existing = backend
                .load_schema_metadata(&name)
                .await
                .map_err(ForgeError::from)?;

            if let Some(existing_def) = existing {
                registry.insert(name_str, existing_def).await;
                continue;
            }

            let plan = DiffEngine::create_new(&definition);

            backend
                .apply_migration(&name, &plan.steps)
                .await
                .map_err(ForgeError::from)?;

            backend
                .store_schema_metadata(&definition)
                .await
                .map_err(ForgeError::from)?;

            registry.insert(name_str, definition).await;
        }
    }

    Ok(())
}

/// Seed system schemas directly into a `HashMap<String, SchemaDefinition>`.
///
/// Actor-compatible variant of [`seed_system_schemas`].
pub async fn seed_system_schemas_into_map(
    registry: &mut HashMap<String, SchemaDefinition>,
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

            let existing = backend
                .load_schema_metadata(&name)
                .await
                .map_err(ForgeError::from)?;

            if let Some(existing_def) = existing {
                registry.insert(name_str, existing_def);
                continue;
            }

            let plan = DiffEngine::create_new(&definition);

            backend
                .apply_migration(&name, &plan.steps)
                .await
                .map_err(ForgeError::from)?;

            backend
                .store_schema_metadata(&definition)
                .await
                .map_err(ForgeError::from)?;

            registry.insert(name_str, definition);
        }
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
    fn user_schema_has_role_rank_field() {
        let schemas = schema_forge_dsl::parse(system_schemas::USER_SCHEMA).unwrap();
        let user = &schemas[0];
        assert!(
            user.fields.iter().any(|f| f.name.as_str() == "role_rank"),
            "User schema must have a role_rank field for Cedar enforcement"
        );
    }

    #[test]
    fn all_system_schemas_returns_three() {
        assert_eq!(system_schemas::all_system_schemas().len(), 3);
    }

    #[test]
    fn dependency_order_user_before_tenant_membership() {
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
        assert_eq!(parsed[0], "User");
        assert_eq!(parsed[1], "TenantMembership");
        assert_eq!(parsed[2], "WebhookSubscription");
    }
}
