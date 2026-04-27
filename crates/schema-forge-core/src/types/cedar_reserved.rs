//! Names reserved by the Cedar policy engine.
//!
//! SchemaForge generates Cedar policies from schema definitions. Some names
//! collide with the Cedar grammar (keywords) or with the namespaces SchemaForge
//! itself owns in generated policies. Allowing such names through would produce
//! Cedar source that fails to parse, evaluates ambiguously, or shadows
//! SchemaForge's own entity types.
//!
//! This module is the single source of truth for the reserved sets. Both
//! `SchemaName::new` and `FieldName::new` consult it, the DSL parser surfaces
//! the same diagnostic, and the Cedar policy generator relies on the
//! invariant that no validated name appears here.

/// Names SchemaForge reserves for its own Cedar namespace and entity types.
///
/// Generated policies live under the `Forge::` Cedar namespace and reference
/// `Forge::Principal`, `Forge::Group`, and `Forge::Schema`. A user-defined
/// schema named the same as our namespace or one of its types would shadow
/// SchemaForge's own references during policy evaluation.
pub const RESERVED_SCHEMA_NAMES: &[&str] = &[
    "Forge",
    "SchemaForge",
    "Principal",
    "Cedar",
];

/// Cedar grammar keywords that cannot appear as Cedar attribute identifiers.
///
/// Field names become attribute names on Cedar resources (`resource.<field>`).
/// Cedar's parser reserves these tokens, and using one as an attribute name
/// either fails to parse in custom policies or produces evaluation ambiguities
/// indistinguishable from policy bugs at audit time.
pub const RESERVED_FIELD_NAMES: &[&str] = &[
    "action",
    "context",
    "else",
    "false",
    "forbid",
    "has",
    "if",
    "in",
    "is",
    "like",
    "permit",
    "principal",
    "resource",
    "then",
    "true",
    "unless",
    "when",
];

/// Returns `Some(reserved_name)` if `s` matches a reserved schema name.
pub fn reserved_schema_name(s: &str) -> Option<&'static str> {
    RESERVED_SCHEMA_NAMES.iter().copied().find(|r| *r == s)
}

/// Returns `Some(reserved_name)` if `s` matches a reserved field name.
pub fn reserved_field_name(s: &str) -> Option<&'static str> {
    RESERVED_FIELD_NAMES.iter().copied().find(|r| *r == s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_name_reserved_list_is_sorted_and_unique() {
        let mut sorted: Vec<&str> = RESERVED_SCHEMA_NAMES.to_vec();
        sorted.sort_unstable();
        let original: Vec<&str> = RESERVED_SCHEMA_NAMES.to_vec();
        assert_eq!(sorted.len(), original.len());
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), sorted.len(), "duplicates in reserved schema names");
    }

    #[test]
    fn field_name_reserved_list_is_sorted_and_unique() {
        let mut sorted: Vec<&str> = RESERVED_FIELD_NAMES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, RESERVED_FIELD_NAMES, "RESERVED_FIELD_NAMES must be sorted");
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), sorted.len(), "duplicates in reserved field names");
    }

    #[test]
    fn reserved_schema_name_detects_known_collisions() {
        assert_eq!(reserved_schema_name("Forge"), Some("Forge"));
        assert_eq!(reserved_schema_name("Principal"), Some("Principal"));
    }

    #[test]
    fn reserved_schema_name_passes_through_app_names() {
        for ok in ["User", "Group", "Schema", "Action", "Contact", "Order"] {
            assert!(reserved_schema_name(ok).is_none(), "should permit {ok}");
        }
    }

    #[test]
    fn reserved_field_name_detects_cedar_keywords() {
        for kw in [
            "permit", "forbid", "principal", "action", "resource", "context",
            "when", "unless", "if", "then", "else", "in", "has", "like", "is",
            "true", "false",
        ] {
            assert!(reserved_field_name(kw).is_some(), "{kw} must be reserved");
        }
    }

    #[test]
    fn reserved_field_name_passes_through_normal_fields() {
        for ok in ["name", "email", "first_name", "created_by", "owner_id", "status"] {
            assert!(reserved_field_name(ok).is_none(), "should permit {ok}");
        }
    }
}
