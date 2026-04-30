use serde::{Deserialize, Serialize};

use crate::authz::principal_claims::PrincipalClaimsConfig;

/// Custom configuration for SchemaForge.
///
/// This is the `T` in `Config<T>`. It gets deserialized from the
/// `[schema_forge]` section of config.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchemaForgeConfig {
    /// SchemaForge-specific settings.
    #[serde(default)]
    pub schema_forge: SchemaForgeSettings,
}

/// SchemaForge settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaForgeSettings {
    /// The URL path prefix for SchemaForge routes (default: "/forge").
    #[serde(default = "default_route_prefix")]
    pub route_prefix: String,

    /// Whether to auto-generate Cedar policy templates when schemas are registered.
    #[serde(default)]
    pub auto_generate_cedar_policies: bool,

    /// Webhook notification settings.
    #[serde(default)]
    pub webhooks: crate::webhook::WebhookConfig,

    /// Lifecycle hook settings.
    #[serde(default)]
    pub hooks: crate::hooks::HooksConfig,

    /// S3-compatible storage backends for `file` field types.
    #[serde(default)]
    pub storage: crate::storage::StorageConfig,

    /// Authorization configuration. Currently exposes operator-defined
    /// PASETO custom-claim → Cedar `Forge::Principal` attribute mappings;
    /// see [`crate::authz::principal_claims`].
    #[serde(default)]
    pub authz: AuthzConfig,
}

/// `[schema_forge.authz]` section of config.toml.
///
/// Holds operator-defined extensions to the authz pipeline. Currently houses
/// only [`AuthzConfig::principal_claims`] but kept as its own section so
/// future authz knobs (custom-policy reload cadence, audit-sink override,
/// etc.) have a stable home.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthzConfig {
    /// Map of Cedar attribute name → claim mapping. Empty by default;
    /// populated from `[schema_forge.authz.principal_claims.<attr>]`
    /// subsections.
    #[serde(default)]
    pub principal_claims: PrincipalClaimsConfig,
}

fn default_route_prefix() -> String {
    "/forge".to_string()
}

impl Default for SchemaForgeSettings {
    fn default() -> Self {
        Self {
            route_prefix: default_route_prefix(),
            auto_generate_cedar_policies: false,
            webhooks: crate::webhook::WebhookConfig::default(),
            hooks: crate::hooks::HooksConfig::default(),
            storage: crate::storage::StorageConfig::default(),
            authz: AuthzConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_correct() {
        let config = SchemaForgeConfig::default();
        assert_eq!(config.schema_forge.route_prefix, "/forge");
        assert!(!config.schema_forge.auto_generate_cedar_policies);
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let config = SchemaForgeConfig {
            schema_forge: SchemaForgeSettings {
                route_prefix: "/api/forge".to_string(),
                auto_generate_cedar_policies: true,
                webhooks: crate::webhook::WebhookConfig::default(),
                hooks: crate::hooks::HooksConfig::default(),
                storage: crate::storage::StorageConfig::default(),
                authz: AuthzConfig::default(),
            },
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: SchemaForgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_forge.route_prefix, "/api/forge");
        assert!(back.schema_forge.auto_generate_cedar_policies);
        assert!(back.schema_forge.authz.principal_claims.is_empty());
    }

    #[test]
    fn principal_claims_section_deserialises() {
        let toml = r#"
            [schema_forge.authz.principal_claims.client_org_id]
            type = "string"

            [schema_forge.authz.principal_claims.team_ids]
            type = "set_of_string"
            required = false

            [schema_forge.authz.principal_claims.level]
            type = "long"
            default = 0
        "#;
        let config: SchemaForgeConfig = toml::from_str(toml).unwrap();
        let claims = &config.schema_forge.authz.principal_claims;
        assert_eq!(claims.len(), 3);
        assert!(claims.contains_key("client_org_id"));
        assert!(claims.contains_key("team_ids"));
        assert!(claims.contains_key("level"));
    }

    #[test]
    fn custom_values_override_defaults() {
        let json = r#"{"schema_forge": {"route_prefix": "/custom", "auto_generate_cedar_policies": true}}"#;
        let config: SchemaForgeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.schema_forge.route_prefix, "/custom");
        assert!(config.schema_forge.auto_generate_cedar_policies);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let json = r#"{}"#;
        let config: SchemaForgeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.schema_forge.route_prefix, "/forge");
        assert!(!config.schema_forge.auto_generate_cedar_policies);
    }

    #[test]
    fn partial_override() {
        let json = r#"{"schema_forge": {"auto_generate_cedar_policies": true}}"#;
        let config: SchemaForgeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.schema_forge.route_prefix, "/forge");
        assert!(config.schema_forge.auto_generate_cedar_policies);
    }
}
