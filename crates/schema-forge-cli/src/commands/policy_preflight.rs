//! Compile the proposed startup bundle before any application schema writes.

use std::path::Path;

use schema_forge_acton::authz::PolicyStoreSnapshot;
use schema_forge_core::types::SchemaDefinition;

use crate::error::CliError;

pub(super) fn compile(
    schemas: &[SchemaDefinition],
    current: &PolicyStoreSnapshot,
    custom_dir: Option<&Path>,
) -> Result<PolicyStoreSnapshot, CliError> {
    PolicyStoreSnapshot::from_schemas(
        schemas,
        custom_dir,
        current.role_ranks.clone(),
        current.principal_claims.clone(),
    )
    .map_err(|error| CliError::Server {
        message: format!("Cedar policy preflight failed before schema changes: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_acton::authz::{PrincipalClaimMappings, RoleRanks};

    #[test]
    fn proposed_bundle_keeps_custom_policy_contract_and_is_ready_for_installation() {
        let old =
            schema_forge_dsl::parse("schema Thing { code: text required label: text }").unwrap();
        let new =
            schema_forge_dsl::parse("schema Thing { renamed: text required label: text }").unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("contract.cedar"),
            r#"forbid (principal is Forge::Principal, action == Action::"ReadThing", resource is Thing) when { resource.code == "secret" };"#).unwrap();
        let current = PolicyStoreSnapshot::from_schemas(
            &old,
            Some(directory.path()),
            RoleRanks::empty(),
            PrincipalClaimMappings::default(),
        )
        .unwrap();
        assert!(compile(&new, &current, Some(directory.path())).is_err());
        // A coordinated policy/schema rename is valid even though the new
        // policy would not compile against the old stored registry.
        std::fs::write(directory.path().join("contract.cedar"),
            r#"forbid (principal is Forge::Principal, action == Action::"ReadThing", resource is Thing) when { resource.renamed == "secret" };"#).unwrap();
        assert!(compile(&new, &current, Some(directory.path())).is_ok());
        std::fs::write(directory.path().join("contract.cedar"),
            r#"forbid (principal is Forge::Principal, action == Action::"ReadThing", resource is Thing) when { resource.code == "secret" };"#).unwrap();
        let prepared = compile(&old, &current, Some(directory.path())).unwrap();
        assert_eq!(prepared.policy_hash, current.policy_hash);
        // The caller installs this exact snapshot after migration; a subsequent
        // disk edit cannot silently change the bundle it already validated.
        std::fs::write(directory.path().join("contract.cedar"), "invalid policy").unwrap();
        let store = schema_forge_acton::authz::PolicyStore::new(current);
        store.swap(prepared);
        assert!(store.current().policy_count > 0);
    }
}
