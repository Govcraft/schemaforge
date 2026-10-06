//! Operator-defined tenant onboarding grants.
use std::fmt;

use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::types::{EntityId, SchemaName};
use serde::{Deserialize, Serialize};

use crate::{
    authz::{role_ranks::PLATFORM_ADMIN_ROLE, RoleRanks},
    oauth_config::{OAuthSettings, SignupPolicy},
};

/// `[schema_forge.tenancy]` onboarding settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TenancySettings {
    /// Role granted to an authenticated creator of a tenant root.
    pub creator_role: Option<String>,
    /// Existing root tenant receiving new, non-invited OAuth accounts.
    pub default_tenant: Option<DefaultTenant>,
}

/// An existing tenant root and the scoped membership role assigned at signup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultTenant {
    /// Configured root schema name.
    pub schema: SchemaName,
    /// Existing root entity TypeID.
    pub id: EntityId,
    /// Scoped role; defaults to `member` independently of OAuth global roles.
    #[serde(default = "default_member_role")]
    pub role: String,
}

fn default_member_role() -> String {
    "member".into()
}

/// Invalid tenant onboarding configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenancySettingsError {
    /// Automatic membership grants require a registered non-platform role.
    InvalidRole { role: String },
    /// A default tenant must belong to the configured root schema.
    InvalidDefaultRoot,
    /// Open signup must provide membership before issuing a tenant session.
    OpenSignupRequiresDefaultTenant,
}

impl fmt::Display for TenancySettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRole { role } => write!(formatter, "tenant onboarding role '{role}' must be nonempty, registered in role_ranks.toml, and cannot be platform_admin"),
            Self::InvalidDefaultRoot => formatter.write_str("schema_forge.tenancy.default_tenant.schema must name the configured tenant root"),
            Self::OpenSignupRequiresDefaultTenant => formatter.write_str("open OAuth signup with tenancy requires schema_forge.tenancy.default_tenant pointing to an existing root; creator_role alone cannot provide a first login membership"),
        }
    }
}
impl std::error::Error for TenancySettingsError {}

impl TenancySettings {
    /// Validate settings against the proposed schema hierarchy before serving.
    /// The caller must separately verify that the default root entity exists.
    pub fn validate(
        &self,
        tenants: &TenantConfig,
        ranks: &RoleRanks,
        oauth: &OAuthSettings,
    ) -> Result<(), TenancySettingsError> {
        for role in self
            .creator_role
            .iter()
            .chain(self.default_tenant.iter().map(|tenant| &tenant.role))
        {
            if role.trim().is_empty() || role == PLATFORM_ADMIN_ROLE || ranks.get(role).is_none() {
                return Err(TenancySettingsError::InvalidRole { role: role.clone() });
            }
        }
        if let Some(default) = &self.default_tenant {
            if tenants.root_schema.as_ref() != Some(&default.schema) {
                return Err(TenancySettingsError::InvalidDefaultRoot);
            }
        }
        if tenants.is_enabled()
            && oauth.enabled
            && oauth.signup == SignupPolicy::Open
            && self.default_tenant.is_none()
        {
            return Err(TenancySettingsError::OpenSignupRequiresDefaultTenant);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenants() -> TenantConfig {
        TenantConfig {
            root_schema: Some(SchemaName::new("Organization").unwrap()),
            hierarchy: vec![],
        }
    }
    fn ranks() -> RoleRanks {
        RoleRanks::from_toml_str("[roles]\nmember = 10\nowner = 20").unwrap()
    }
    fn default_tenant() -> DefaultTenant {
        DefaultTenant {
            schema: SchemaName::new("Organization").unwrap(),
            id: EntityId::new("organization"),
            role: "member".into(),
        }
    }

    #[test]
    fn open_signup_requires_existing_tenant_configuration_even_with_creator_role() {
        let settings = TenancySettings {
            creator_role: Some("owner".into()),
            ..Default::default()
        };
        let mut oauth = OAuthSettings {
            enabled: true,
            signup: SignupPolicy::Open,
            ..Default::default()
        };
        assert_eq!(
            settings.validate(&tenants(), &ranks(), &oauth),
            Err(TenancySettingsError::OpenSignupRequiresDefaultTenant)
        );
        oauth.signup = SignupPolicy::InviteOnly;
        assert!(settings.validate(&tenants(), &ranks(), &oauth).is_ok());
        oauth.signup = SignupPolicy::Open;
        oauth.enabled = false;
        assert!(settings.validate(&tenants(), &ranks(), &oauth).is_ok());
        assert!(settings
            .validate(
                &TenantConfig {
                    root_schema: None,
                    hierarchy: vec![]
                },
                &ranks(),
                &OAuthSettings::default()
            )
            .is_ok());
    }

    #[test]
    fn rejects_unregistered_empty_and_platform_membership_grants() {
        for role in ["", " ", "unknown", "platform_admin"] {
            let settings = TenancySettings {
                creator_role: Some(role.into()),
                ..Default::default()
            };
            assert!(matches!(
                settings.validate(&tenants(), &ranks(), &OAuthSettings::default()),
                Err(TenancySettingsError::InvalidRole { .. })
            ));
            let settings = TenancySettings {
                default_tenant: Some(DefaultTenant {
                    role: role.into(),
                    ..default_tenant()
                }),
                ..Default::default()
            };
            assert!(matches!(
                settings.validate(&tenants(), &ranks(), &OAuthSettings::default()),
                Err(TenancySettingsError::InvalidRole { .. })
            ));
        }
    }

    #[test]
    fn default_tenant_requires_matching_root_and_valid_typeid() {
        let settings = TenancySettings {
            default_tenant: Some(default_tenant()),
            ..Default::default()
        };
        assert!(settings
            .validate(&tenants(), &ranks(), &OAuthSettings::default())
            .is_ok());
        assert_eq!(
            settings.validate(
                &TenantConfig {
                    root_schema: None,
                    hierarchy: vec![]
                },
                &ranks(),
                &OAuthSettings::default()
            ),
            Err(TenancySettingsError::InvalidDefaultRoot)
        );
        let different_root = TenantConfig {
            root_schema: Some(SchemaName::new("Company").unwrap()),
            hierarchy: vec![],
        };
        assert_eq!(
            settings.validate(&different_root, &ranks(), &OAuthSettings::default()),
            Err(TenancySettingsError::InvalidDefaultRoot)
        );
        let mut json = serde_json::to_value(settings).unwrap();
        json["default_tenant"]
            .as_object_mut()
            .unwrap()
            .remove("role");
        assert_eq!(
            serde_json::from_value::<TenancySettings>(json.clone())
                .unwrap()
                .default_tenant
                .unwrap()
                .role,
            "member"
        );
        json["default_tenant"]["id"] = "invalid-id".into();
        assert!(serde_json::from_value::<TenancySettings>(json).is_err());
    }
}
