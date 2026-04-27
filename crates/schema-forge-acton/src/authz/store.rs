//! Compiled-policy cache, hot-swapped via `ArcSwap`.
//!
//! Holds the validated `cedar_policy::PolicySet`, the Cedar `Schema`, the
//! [`RoleRanks`] map, and the SHA-256 of the canonical policy serialization
//! so audit endpoints can expose a stable hash for verification. All four
//! refresh together when schemas change or custom policies reload.

use std::sync::Arc;

use arc_swap::ArcSwap;
use cedar_policy::{PolicySet, Schema, ValidationMode, Validator};
use sha2::{Digest, Sha256};

use crate::authz::role_ranks::{RoleRanks, RoleRanksError};

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
}

impl PolicyStoreSnapshot {
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
}
