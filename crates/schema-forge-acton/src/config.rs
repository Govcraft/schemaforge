use schema_forge_signing::SigningConfig;
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

    /// Signed-schema enforcement. The CLI builds a
    /// [`schema_forge_signing::VerifyPolicy`] from this section before
    /// loading any `.schema` file, so on-disk tampering and
    /// untrusted-author additions are rejected at the parse boundary.
    /// Defaults to `mode = "off"` so existing deployments keep parsing
    /// today; production setups should switch to `enforce` and list
    /// trust anchors. See `schema-forge-signing` for the file layout
    /// and verifier kinds.
    #[serde(default)]
    pub signing: SigningConfig,

    /// Connection defaults for the `schemaforge` CLI's HTTP commands
    /// (`entity`, `login`). The running service never reads this section —
    /// it exists only so an operator can pin a server URL, token file, and
    /// TLS material once in the shared `config.toml` instead of repeating
    /// `--server`/`--token-file` flags on every invocation. CLI flags and
    /// environment variables still override these values.
    #[serde(default)]
    pub client: ClientConfig,
}

/// `[schema_forge.client]` section of config.toml.
///
/// Read exclusively by the CLI's HTTP command group; the server ignores it.
/// All fields are optional so the CLI can layer environment variables and
/// command-line flags on top with a clear precedence (flags > env > config >
/// built-in defaults).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientConfig {
    /// Base URL of the running instance, e.g. `https://forge.agency.gov`.
    /// The CLI appends the versioned API base path itself.
    #[serde(default)]
    pub server: Option<String>,

    /// Path to a file containing the Bearer PASETO token. Tokens are never
    /// stored inline in config so the file's own permissions guard the
    /// secret.
    #[serde(default)]
    pub token_file: Option<String>,

    /// Path to a PEM-encoded CA certificate for verifying a private-PKI
    /// server certificate.
    #[serde(default)]
    pub ca_cert: Option<String>,

    /// Per-request timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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
            signing: SigningConfig::default(),
            client: ClientConfig::default(),
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
                signing: SigningConfig::default(),
                client: ClientConfig::default(),
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
    fn client_section_deserialises() {
        let toml = r#"
            [schema_forge.client]
            server = "https://forge.agency.gov"
            token_file = "~/.config/schemaforge/token"
            ca_cert = "/etc/pki/root.pem"
            timeout_secs = 45
        "#;
        let config: SchemaForgeConfig = toml::from_str(toml).unwrap();
        let client = &config.schema_forge.client;
        assert_eq!(client.server.as_deref(), Some("https://forge.agency.gov"));
        assert_eq!(
            client.token_file.as_deref(),
            Some("~/.config/schemaforge/token")
        );
        assert_eq!(client.ca_cert.as_deref(), Some("/etc/pki/root.pem"));
        assert_eq!(client.timeout_secs, Some(45));
    }

    #[test]
    fn client_section_defaults_to_empty() {
        let config: SchemaForgeConfig = serde_json::from_str("{}").unwrap();
        let client = &config.schema_forge.client;
        assert!(client.server.is_none());
        assert!(client.token_file.is_none());
        assert!(client.ca_cert.is_none());
        assert!(client.timeout_secs.is_none());
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
