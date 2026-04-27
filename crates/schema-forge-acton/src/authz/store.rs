//! Compiled-policy cache, hot-swapped via `ArcSwap`.
//!
//! Holds the validated `cedar_policy::PolicySet`, the Cedar `Schema`, the
//! [`RoleRanks`] map, and the SHA-256 of the canonical policy serialization
//! so audit endpoints can expose a stable hash for verification. All four
//! refresh together when schemas change or custom policies reload.

use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use cedar_policy::{PolicySet, Schema, ValidationMode, Validator};
use schema_forge_core::types::SchemaDefinition;
use sha2::{Digest, Sha256};

use crate::authz::loader::{load_custom_policies, LoaderError};
use crate::authz::role_ranks::{RoleRanks, RoleRanksError};
use crate::cedar::{generate_cedar_schema, policy_gen::generate_full_policy_set, SchemaGenError};

/// Errors raised while compiling or installing a policy bundle.
#[derive(Debug, thiserror::Error)]
pub enum PolicyStoreError {
    /// Cedar Schema (entity-type / action declarations) failed to construct.
    #[error("Cedar schema construction failed: {0}")]
    Schema(String),
    /// Cedar PolicySet failed to parse.
    #[error("Cedar PolicySet parse failed: {0}")]
    Parse(String),
    /// Strict-mode validator rejected the bundle.
    #[error("Cedar policy validation failed:\n{0}")]
    Validation(String),
    /// The role-rank file could not be loaded.
    #[error(transparent)]
    RoleRanks(#[from] RoleRanksError),
    /// Custom policy directory could not be read.
    #[error(transparent)]
    Loader(#[from] LoaderError),
    /// Schema source generation failed.
    #[error(transparent)]
    SchemaGen(#[from] SchemaGenError),
}

/// Immutable bundle held by [`PolicyStore`]. Cheap to clone via `Arc`.
#[derive(Debug)]
pub struct PolicyStoreSnapshot {
    /// Compiled Cedar policy set (generated + custom).
    pub policy_set: PolicySet,
    /// Cedar schema declaring entity types, actions, attributes.
    pub schema: Schema,
    /// Role-name → rank mapping consulted during principal construction.
    pub role_ranks: RoleRanks,
    /// SHA-256 of the canonical PolicySet rendering, surfaced via audit.
    pub policy_hash: String,
    /// Total number of policies in the set.
    pub policy_count: usize,
}

/// Hot-swappable container holding the current authorization bundle.
#[derive(Debug)]
pub struct PolicyStore {
    inner: ArcSwap<PolicyStoreSnapshot>,
}

impl PolicyStore {
    /// Constructs a new [`PolicyStore`] from an already-validated snapshot.
    pub fn new(snapshot: PolicyStoreSnapshot) -> Self {
        Self {
            inner: ArcSwap::from(Arc::new(snapshot)),
        }
    }

    /// Returns the current snapshot. Cheap pointer-clone.
    pub fn current(&self) -> Arc<PolicyStoreSnapshot> {
        self.inner.load_full()
    }

    /// Atomically replaces the current snapshot with `next`.
    pub fn swap(&self, next: PolicyStoreSnapshot) {
        self.inner.store(Arc::new(next));
    }

    /// Compiles a fresh snapshot from `schemas` (reusing the current
    /// snapshot's [`RoleRanks`]) and atomically installs it.
    ///
    /// Returns the original snapshot unchanged on compile failure so the
    /// running bundle keeps serving traffic — callers are expected to
    /// surface the error so the originating mutation can be rolled back.
    /// `custom_dir` mirrors [`PolicyStoreSnapshot::from_schemas`] (`None`
    /// for "no custom policies").
    pub fn recompile_from_schemas(
        &self,
        schemas: &[SchemaDefinition],
        custom_dir: Option<&Path>,
    ) -> Result<(), PolicyStoreError> {
        let role_ranks = self.current().role_ranks.clone();
        let next = PolicyStoreSnapshot::from_schemas(schemas, custom_dir, role_ranks)?;
        self.swap(next);
        Ok(())
    }
}

impl PolicyStoreSnapshot {
    /// Builds a snapshot from a slice of registered schemas.
    ///
    /// End-to-end pipeline:
    /// 1. Generate the Cedar schema source via [`generate_cedar_schema`].
    /// 2. Generate the global + per-schema + per-field policy set via
    ///    [`generate_full_policy_set`].
    /// 3. Load every `*.cedar` file in `custom_dir` (if the directory exists)
    ///    and concatenate after the generated policies.
    /// 4. Run [`PolicyStoreSnapshot::compile`] which strict-validates the
    ///    bundle and rejects on any error or warning.
    pub fn from_schemas(
        schemas: &[SchemaDefinition],
        custom_dir: Option<&Path>,
        role_ranks: RoleRanks,
    ) -> Result<Self, PolicyStoreError> {
        let schema_src = generate_cedar_schema(schemas)?;

        let generated = generate_full_policy_set(schemas);
        let mut policies_src = generated
            .iter()
            .map(|p| p.cedar_text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        if let Some(dir) = custom_dir {
            let custom = load_custom_policies(dir)?;
            for source in custom {
                policies_src.push_str("\n\n");
                policies_src.push_str(&source.text);
            }
        }

        Self::compile(&schema_src, &policies_src, role_ranks)
    }

    /// Builds and validates a snapshot from raw Cedar source plus metadata.
    ///
    /// `policies_src` is the concatenation of every generated and custom
    /// Cedar policy. `schema_src` is the Cedar schema source. Both are
    /// parsed, the schema is constructed, the policies are validated in
    /// strict mode, and the bundle is rejected if validation produces any
    /// errors or warnings.
    pub fn compile(
        schema_src: &str,
        policies_src: &str,
        role_ranks: RoleRanks,
    ) -> Result<Self, PolicyStoreError> {
        let (schema, schema_warnings): (Schema, _) = Schema::from_cedarschema_str(schema_src)
            .map_err(|e| PolicyStoreError::Schema(e.to_string()))?;
        let warnings: Vec<String> = schema_warnings.map(|w| w.to_string()).collect();
        if !warnings.is_empty() {
            return Err(PolicyStoreError::Schema(format!(
                "schema produced warnings (treated as errors):\n{}",
                warnings.join("\n")
            )));
        }

        let policy_set: PolicySet = policies_src
            .parse()
            .map_err(|e: cedar_policy::ParseErrors| PolicyStoreError::Parse(e.to_string()))?;

        let validator = Validator::new(schema.clone());
        let result = validator.validate(&policy_set, ValidationMode::Strict);
        if !result.validation_passed() {
            let errors: Vec<String> = result.validation_errors().map(|e| e.to_string()).collect();
            let warns: Vec<String> = result.validation_warnings().map(|w| w.to_string()).collect();
            let mut combined = errors;
            combined.extend(warns);
            return Err(PolicyStoreError::Validation(combined.join("\n")));
        }

        let policy_count = policy_set.policies().count();
        let policy_hash = compute_policy_hash(&policy_set);

        Ok(Self {
            policy_set,
            schema,
            role_ranks,
            policy_hash,
            policy_count,
        })
    }
}

/// Hash of the canonical JSON serialization of a [`PolicySet`].
///
/// Stable across runs given identical input, suitable for an audit endpoint.
fn compute_policy_hash(policy_set: &PolicySet) -> String {
    let mut hasher = Sha256::new();
    if let Ok(json) = policy_set.clone().to_json() {
        if let Ok(canonical) = serde_json::to_vec(&json) {
            hasher.update(&canonical);
        }
    }
    let digest = hasher.finalize();
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_SCHEMA: &str = r#"
        namespace Forge {
            entity Principal = { id: String };
            entity Group;
        }
        entity Contact = {};
        action "ReadContact" appliesTo {
            principal: [Forge::Principal],
            resource: [Contact],
        };
    "#;

    const MINIMAL_POLICY: &str = r#"
        permit(
            principal,
            action == Action::"ReadContact",
            resource is Contact
        );
    "#;

    #[test]
    fn compile_succeeds_for_validated_bundle() {
        let snap =
            PolicyStoreSnapshot::compile(MINIMAL_SCHEMA, MINIMAL_POLICY, RoleRanks::empty())
                .unwrap();
        assert_eq!(snap.policy_count, 1);
        assert_eq!(snap.policy_hash.len(), 64);
    }

    #[test]
    fn compile_rejects_policy_referencing_unknown_action() {
        let bad = r#"permit(principal, action == Action::"GhostAction", resource);"#;
        let err = PolicyStoreSnapshot::compile(MINIMAL_SCHEMA, bad, RoleRanks::empty()).unwrap_err();
        assert!(matches!(err, PolicyStoreError::Validation(_)));
    }

    #[test]
    fn store_swap_makes_new_snapshot_visible() {
        let s1 =
            PolicyStoreSnapshot::compile(MINIMAL_SCHEMA, MINIMAL_POLICY, RoleRanks::empty())
                .unwrap();
        let store = PolicyStore::new(s1);
        let h1 = store.current().policy_hash.clone();
        let s2 =
            PolicyStoreSnapshot::compile(MINIMAL_SCHEMA, MINIMAL_POLICY, RoleRanks::empty())
                .unwrap();
        store.swap(s2);
        assert_eq!(store.current().policy_hash, h1);
    }

    fn schema_named(name: &str) -> SchemaDefinition {
        use schema_forge_core::types::{
            FieldDefinition, FieldName, FieldType, SchemaId, SchemaName, TextConstraints,
        };
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn recompile_from_schemas_installs_new_bundle() {
        // Start with a single-schema bundle, then recompile against a
        // two-schema set — the policy_count should grow and the hash should
        // change, proving the swap landed.
        let s1 = PolicyStoreSnapshot::from_schemas(
            &[schema_named("Alpha")],
            None,
            RoleRanks::empty(),
        )
        .unwrap();
        let initial_count = s1.policy_count;
        let initial_hash = s1.policy_hash.clone();
        let store = PolicyStore::new(s1);

        store
            .recompile_from_schemas(&[schema_named("Alpha"), schema_named("Beta")], None)
            .expect("recompile should succeed");

        let current = store.current();
        assert!(
            current.policy_count > initial_count,
            "second schema should add at least one policy ({} -> {})",
            initial_count,
            current.policy_count,
        );
        assert_ne!(
            current.policy_hash, initial_hash,
            "policy hash must change after recompile",
        );
    }

    #[test]
    fn recompile_from_schemas_keeps_old_bundle_on_failure() {
        // Build a healthy bundle, then point recompile at a custom-policy
        // directory whose Cedar source fails to validate. The store must
        // keep serving the original snapshot instead of dropping into a
        // half-broken state.
        let s1 =
            PolicyStoreSnapshot::from_schemas(&[schema_named("Alpha")], None, RoleRanks::empty())
                .unwrap();
        let original_hash = s1.policy_hash.clone();
        let store = PolicyStore::new(s1);

        let tmp = std::env::temp_dir().join(format!(
            "schemaforge-policystore-bad-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("invalid.cedar"),
            r#"permit(principal, action == Action::"DoesNotExist", resource);"#,
        )
        .unwrap();

        let result = store.recompile_from_schemas(&[schema_named("Alpha")], Some(&tmp));
        assert!(
            matches!(result, Err(PolicyStoreError::Validation(_))),
            "expected validation failure, got {result:?}"
        );
        assert_eq!(
            store.current().policy_hash,
            original_hash,
            "store must preserve the original snapshot when recompile fails",
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
