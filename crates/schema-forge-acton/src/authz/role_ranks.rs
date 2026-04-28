//! Role-name → rank mapping driving SchemaForge's role hierarchy.
//!
//! Cedar policies enforce the no-upward-visibility/creation rule for user
//! management by comparing `principal.role_rank` against `resource.role_rank`.
//! Role names are arbitrary strings declared either by the policy generator
//! (built-in roles) or by operators (custom roles). The numeric rank for each
//! role lives in `policies/role_ranks.toml` so the gov-audit story can track
//! both the policy text and the hierarchy that gates it in version control.
//!
//! `platform_admin` always has rank [`PLATFORM_ADMIN_RANK`] and the loader
//! refuses any attempt to override it. Roles referenced by Cedar policies but
//! absent from this map are surfaced as a compile-time error during policy
//! validation, mirroring the secure-by-default posture of the rest of the
//! engine.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Rank assigned to the platform superuser role.
///
/// Hardcoded to `i64::MAX` so no operator-supplied rank can equal or exceed
/// it. `platform_admin` is always at the top of the hierarchy.
pub const PLATFORM_ADMIN_RANK: i64 = i64::MAX;

/// The reserved role name representing the platform superuser.
pub const PLATFORM_ADMIN_ROLE: &str = "platform_admin";

/// Rank value for a single role.
pub type RoleRank = i64;

/// Errors that can occur while loading or validating a [`RoleRanks`] map.
#[derive(Debug, thiserror::Error)]
pub enum RoleRanksError {
    /// The role-ranks file does not exist at the expected path.
    ///
    /// Distinct from [`RoleRanksError::Io`] so callers (and operators) can
    /// tell a missing-config problem from an unreadable-config problem
    /// without parsing error strings. SchemaForge refuses to start without
    /// an explicit hierarchy because a silently-empty rank map would let
    /// the runtime accept any role name with rank 0 and still pass Cedar's
    /// `user_role_rank_forbid` checks for non-platform-admin users.
    #[error(
        "role ranks file '{path}' does not exist; create it (even with an \
         empty `[roles]` table) so the policy hierarchy is explicit"
    )]
    FileNotFound { path: String },
    /// The TOML file could not be read from disk.
    #[error("failed to read role ranks file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The TOML file failed to parse.
    #[error("failed to parse role ranks TOML at '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    /// An operator attempted to override the reserved `platform_admin` rank.
    #[error(
        "role 'platform_admin' is reserved by SchemaForge and cannot appear in role_ranks.toml \
         (it is always rank i64::MAX)"
    )]
    PlatformAdminOverride,
    /// A rank value was outside the permitted range.
    #[error(
        "role '{role}' has rank {rank} but ranks must be non-negative and below \
         i64::MAX (which is reserved for platform_admin)"
    )]
    InvalidRank { role: String, rank: i64 },
    /// A role name was empty.
    #[error("role names must be non-empty strings")]
    EmptyRoleName,
    /// A Cedar policy referenced a role name with no rank entry.
    #[error(
        "role '{role}' is referenced by a Cedar policy but is missing from role_ranks.toml; \
         add it with an explicit rank or remove the policy"
    )]
    UnregisteredRole { role: String },
}

/// Validated role-name → rank mapping.
///
/// Always contains an entry for `platform_admin` at rank [`PLATFORM_ADMIN_RANK`].
/// Construct via [`RoleRanks::from_toml_file`], [`RoleRanks::from_toml_str`],
/// or [`RoleRanks::empty`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRanks {
    ranks: BTreeMap<String, RoleRank>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoleRanksToml {
    #[serde(default)]
    roles: BTreeMap<String, i64>,
}

impl RoleRanks {
    /// Constructs an empty rank map containing only `platform_admin`.
    pub fn empty() -> Self {
        let mut ranks = BTreeMap::new();
        ranks.insert(PLATFORM_ADMIN_ROLE.to_string(), PLATFORM_ADMIN_RANK);
        Self { ranks }
    }

    /// Loads ranks from a TOML file at `path`.
    ///
    /// A missing file is a hard error ([`RoleRanksError::FileNotFound`]).
    /// Operators must provision the file explicitly — even an empty
    /// `[roles]` table is acceptable — so a deployment never silently
    /// runs with no role hierarchy. Use [`RoleRanks::empty`] directly in
    /// tests or programmatic constructors that don't go through disk.
    pub fn from_toml_file(path: &Path) -> Result<Self, RoleRanksError> {
        let path_str = path.display().to_string();
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(RoleRanksError::FileNotFound { path: path_str })
            }
            Err(e) => {
                return Err(RoleRanksError::Io {
                    path: path_str,
                    source: e,
                })
            }
        };
        Self::from_toml_str_with_path(&contents, &path_str)
    }

    /// Parses ranks from a TOML string with a synthetic path used in errors.
    pub fn from_toml_str(input: &str) -> Result<Self, RoleRanksError> {
        Self::from_toml_str_with_path(input, "<inline>")
    }

    fn from_toml_str_with_path(input: &str, path: &str) -> Result<Self, RoleRanksError> {
        let parsed: RoleRanksToml =
            toml::from_str(input).map_err(|source| RoleRanksError::Parse {
                path: path.to_string(),
                source,
            })?;

        let mut ranks = BTreeMap::new();
        ranks.insert(PLATFORM_ADMIN_ROLE.to_string(), PLATFORM_ADMIN_RANK);

        for (role, rank) in parsed.roles {
            if role.is_empty() {
                return Err(RoleRanksError::EmptyRoleName);
            }
            if role == PLATFORM_ADMIN_ROLE {
                return Err(RoleRanksError::PlatformAdminOverride);
            }
            if rank < 0 || rank == PLATFORM_ADMIN_RANK {
                return Err(RoleRanksError::InvalidRank { role, rank });
            }
            ranks.insert(role, rank);
        }

        Ok(Self { ranks })
    }

    /// Returns the rank for `role`, if registered.
    pub fn get(&self, role: &str) -> Option<RoleRank> {
        self.ranks.get(role).copied()
    }

    /// Returns the maximum rank across `roles`, or `0` if none are
    /// registered. Unknown role names contribute `0` (the default
    /// least-privileged rank).
    pub fn max_rank(&self, roles: &[String]) -> RoleRank {
        roles
            .iter()
            .map(|r| self.get(r).unwrap_or(0))
            .max()
            .unwrap_or(0)
    }

    /// Returns the set of all registered role names, including
    /// `platform_admin`.
    pub fn role_names(&self) -> impl Iterator<Item = &str> {
        self.ranks.keys().map(String::as_str)
    }

    /// Asserts that every role in `referenced` is registered. Returns the
    /// first unregistered role as an error. Used during policy validation.
    pub fn ensure_all_registered<'a, I>(&self, referenced: I) -> Result<(), RoleRanksError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        for role in referenced {
            if !self.ranks.contains_key(role) {
                return Err(RoleRanksError::UnregisteredRole {
                    role: role.to_string(),
                });
            }
        }
        Ok(())
    }
}

impl Default for RoleRanks {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_contains_only_platform_admin() {
        let ranks = RoleRanks::empty();
        assert_eq!(ranks.get(PLATFORM_ADMIN_ROLE), Some(PLATFORM_ADMIN_RANK));
        assert_eq!(ranks.role_names().count(), 1);
    }

    #[test]
    fn parses_simple_toml() {
        let toml = r#"
            [roles]
            org_admin = 800
            manager   = 500
            member    = 100
        "#;
        let ranks = RoleRanks::from_toml_str(toml).unwrap();
        assert_eq!(ranks.get("org_admin"), Some(800));
        assert_eq!(ranks.get("manager"), Some(500));
        assert_eq!(ranks.get("member"), Some(100));
        assert_eq!(ranks.get(PLATFORM_ADMIN_ROLE), Some(PLATFORM_ADMIN_RANK));
    }

    #[test]
    fn rejects_platform_admin_override() {
        let toml = "[roles]\nplatform_admin = 1\n";
        let err = RoleRanks::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, RoleRanksError::PlatformAdminOverride));
    }

    #[test]
    fn rejects_negative_rank() {
        let toml = "[roles]\nlow = -1\n";
        let err = RoleRanks::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, RoleRanksError::InvalidRank { .. }));
    }

    #[test]
    fn rejects_max_rank_for_non_admin() {
        // i64::MAX is reserved for platform_admin
        let toml = format!("[roles]\noverlord = {}\n", i64::MAX);
        let err = RoleRanks::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, RoleRanksError::InvalidRank { .. }));
    }

    #[test]
    fn missing_file_is_hard_error() {
        let path = std::path::PathBuf::from("/tmp/definitely-does-not-exist-role-ranks.toml");
        let err = RoleRanks::from_toml_file(&path).unwrap_err();
        match err {
            RoleRanksError::FileNotFound { path: reported } => {
                assert!(reported.contains("definitely-does-not-exist"));
            }
            other => panic!("expected FileNotFound, got {other:?}"),
        }
    }

    #[test]
    fn max_rank_picks_highest_role() {
        let toml = r#"[roles]
manager = 500
member = 100
"#;
        let ranks = RoleRanks::from_toml_str(toml).unwrap();
        assert_eq!(
            ranks.max_rank(&["member".into(), "manager".into()]),
            500
        );
    }

    #[test]
    fn max_rank_unknown_role_is_zero() {
        let ranks = RoleRanks::empty();
        assert_eq!(ranks.max_rank(&["someone".into()]), 0);
    }

    #[test]
    fn max_rank_platform_admin_dominates() {
        let toml = "[roles]\nmanager = 500\n";
        let ranks = RoleRanks::from_toml_str(toml).unwrap();
        assert_eq!(
            ranks.max_rank(&["manager".into(), PLATFORM_ADMIN_ROLE.into()]),
            PLATFORM_ADMIN_RANK
        );
    }

    #[test]
    fn ensure_all_registered_reports_first_missing() {
        let toml = "[roles]\nmember = 1\n";
        let ranks = RoleRanks::from_toml_str(toml).unwrap();
        let referenced = vec!["member", "manager", "ghost"];
        let err = ranks
            .ensure_all_registered(referenced)
            .unwrap_err();
        match err {
            RoleRanksError::UnregisteredRole { role } => assert_eq!(role, "manager"),
            other => panic!("expected UnregisteredRole, got {other:?}"),
        }
    }

    #[test]
    fn ensure_all_registered_passes_when_all_present() {
        let toml = "[roles]\nmanager = 1\nmember = 2\n";
        let ranks = RoleRanks::from_toml_str(toml).unwrap();
        ranks
            .ensure_all_registered(["manager", "member", PLATFORM_ADMIN_ROLE])
            .unwrap();
    }
}
