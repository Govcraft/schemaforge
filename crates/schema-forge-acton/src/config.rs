use serde::{Deserialize, Serialize};

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
}

fn default_route_prefix() -> String {
    "/forge".to_string()
}

impl Default for SchemaForgeSettings {
    fn default() -> Self {
        Self {
            route_prefix: default_route_prefix(),
            auto_generate_cedar_policies: false,
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
            },
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: SchemaForgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_forge.route_prefix, "/api/forge");
        assert!(back.schema_forge.auto_generate_cedar_policies);
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
