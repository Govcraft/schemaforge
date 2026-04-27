//! Property tests for the security-critical invariants the runtime
//! relies on for fail-closed behavior:
//!
//! 1. `Entity::strip_hidden` always drops every `@hidden` field present in
//!    the schema and never drops a non-`@hidden` field that's defined in the
//!    schema. (Unknown fields pass through; that's documented behavior.)
//! 2. `compute_role_rank` returns the maximum rank across the supplied
//!    roles, mapping unregistered roles to `0`, and produces `0` for the
//!    empty role list.
//!
//! These two invariants gate the secret-handling story (`@hidden`
//! `password_hash` never escapes the storage boundary) and the
//! no-upward-visibility user-mgmt rule (`principal.role_rank >=
//! resource.role_rank`). Property tests defend them against unintentional
//! regressions far better than scattershot unit cases.

use std::collections::{BTreeMap, HashMap};

use proptest::prelude::*;

use schema_forge_backend::{compute_role_rank, entity::Entity};
use schema_forge_core::types::{
    DynamicValue, FieldAnnotation, FieldDefinition, FieldName, FieldType, SchemaDefinition,
    SchemaId, SchemaName,
};

// ---------------------------------------------------------------------------
// `Entity::strip_hidden`
// ---------------------------------------------------------------------------

/// Pool of valid snake_case field names. Picking from a fixed pool keeps the
/// strategies simple and ensures the entity / schema can share field names.
const FIELD_NAME_POOL: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    "india", "juliet", "kilo", "lima", "mike", "november",
];

/// Strategy: pick a non-empty subset of field names from `FIELD_NAME_POOL`
/// and decide independently whether each is `@hidden`.
fn schema_strategy() -> impl Strategy<Value = Vec<(String, bool)>> {
    proptest::collection::vec(0..FIELD_NAME_POOL.len(), 1..=FIELD_NAME_POOL.len())
        .prop_map(|indices| {
            // Dedup while preserving order so the resulting field names are unique.
            let mut seen = std::collections::HashSet::new();
            indices
                .into_iter()
                .filter(|i| seen.insert(*i))
                .map(|i| FIELD_NAME_POOL[i].to_string())
                .collect::<Vec<_>>()
        })
        .prop_filter("at least one field", |fields| !fields.is_empty())
        .prop_flat_map(|fields| {
            let len = fields.len();
            (Just(fields), proptest::collection::vec(any::<bool>(), len))
        })
        .prop_map(|(fields, hidden_mask)| {
            fields
                .into_iter()
                .zip(hidden_mask)
                .collect::<Vec<_>>()
        })
}

fn build_schema(fields_with_hidden: &[(String, bool)]) -> SchemaDefinition {
    let fields = fields_with_hidden
        .iter()
        .map(|(name, hidden)| {
            let mut annotations = Vec::new();
            if *hidden {
                annotations.push(FieldAnnotation::Hidden);
            }
            FieldDefinition::with_annotations(
                FieldName::new(name).expect("valid name from fixed pool"),
                FieldType::Text(Default::default()),
                Vec::new(),
                annotations,
            )
        })
        .collect();
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("ProptestEntity").expect("valid schema name"),
        fields,
        Vec::new(),
    )
    .expect("valid schema")
}

fn build_entity(schema: &SchemaDefinition, populated_fields: &[String]) -> Entity {
    let mut fields = BTreeMap::new();
    for name in populated_fields {
        fields.insert(name.clone(), DynamicValue::Text(format!("v_{name}")));
    }
    Entity::new(schema.name.clone(), fields)
}

proptest! {
    /// After `strip_hidden`, no remaining field is `@hidden` according to the
    /// schema. (Fields not present in the schema pass through — that's
    /// documented "schema drift" behavior; we assert about schema-known
    /// fields only.)
    #[test]
    fn strip_hidden_removes_every_hidden_field(
        schema_fields in schema_strategy(),
        populate_mask in proptest::collection::vec(any::<bool>(), 1..=FIELD_NAME_POOL.len()),
    ) {
        let schema = build_schema(&schema_fields);

        // Populate the entity with whichever schema fields the mask selects.
        // Pad/truncate the mask to match the schema field count so we can zip
        // safely.
        let populated: Vec<String> = schema_fields
            .iter()
            .zip(populate_mask.iter().cycle())
            .filter(|&(_, keep)| *keep)
            .map(|((name, _), _)| name.clone())
            .collect();

        let mut entity = build_entity(&schema, &populated);
        entity.strip_hidden(&schema);

        for name in entity.fields.keys() {
            if let Some(field) = schema.field(name) {
                prop_assert!(
                    !field.is_hidden(),
                    "@hidden field '{name}' survived strip_hidden"
                );
            }
        }
    }

    /// `strip_hidden` is idempotent: applying it twice produces the same
    /// field set as applying it once.
    #[test]
    fn strip_hidden_is_idempotent(
        schema_fields in schema_strategy(),
        populate_mask in proptest::collection::vec(any::<bool>(), 1..=FIELD_NAME_POOL.len()),
    ) {
        let schema = build_schema(&schema_fields);
        let populated: Vec<String> = schema_fields
            .iter()
            .zip(populate_mask.iter().cycle())
            .filter(|&(_, keep)| *keep)
            .map(|((name, _), _)| name.clone())
            .collect();

        let mut once = build_entity(&schema, &populated);
        once.strip_hidden(&schema);
        let mut twice = once.clone();
        twice.strip_hidden(&schema);

        prop_assert_eq!(once.fields, twice.fields);
    }

    /// `strip_hidden` preserves every non-`@hidden` schema field that was
    /// present on the entity before the call. This is the dual of the
    /// "removes every hidden field" property and rules out an over-eager
    /// implementation that drops too much.
    #[test]
    fn strip_hidden_preserves_non_hidden_fields(
        schema_fields in schema_strategy(),
        populate_mask in proptest::collection::vec(any::<bool>(), 1..=FIELD_NAME_POOL.len()),
    ) {
        let schema = build_schema(&schema_fields);
        let populated: Vec<String> = schema_fields
            .iter()
            .zip(populate_mask.iter().cycle())
            .filter(|&(_, keep)| *keep)
            .map(|((name, _), _)| name.clone())
            .collect();

        let mut entity = build_entity(&schema, &populated);
        let before = entity.fields.clone();
        entity.strip_hidden(&schema);

        for (name, value) in &before {
            let is_hidden = schema.field(name).is_some_and(|f| f.is_hidden());
            if !is_hidden {
                prop_assert_eq!(
                    entity.fields.get(name),
                    Some(value),
                    "non-hidden field '{}' was dropped by strip_hidden",
                    name
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `compute_role_rank`
// ---------------------------------------------------------------------------

const ROLE_NAME_POOL: &[&str] =
    &["member", "manager", "admin", "auditor", "viewer", "operator"];

/// Strategy: a rank table mapping a subset of role names to small positive
/// ranks (avoiding `i64::MAX` which is reserved for `platform_admin`).
fn rank_table_strategy() -> impl Strategy<Value = HashMap<String, i64>> {
    proptest::collection::vec((0..ROLE_NAME_POOL.len(), 1i64..1_000_000_000), 0..=6).prop_map(
        |entries| {
            let mut map = HashMap::new();
            for (idx, rank) in entries {
                map.insert(ROLE_NAME_POOL[idx].to_string(), rank);
            }
            map
        },
    )
}

/// Strategy: a (possibly empty, possibly with-duplicates) list of role names
/// drawn from the pool plus optional "ghost" roles that won't appear in the
/// rank table.
fn role_list_strategy() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        proptest::sample::select(
            ROLE_NAME_POOL
                .iter()
                .map(|s| s.to_string())
                .chain(["ghost".to_string(), "unknown_role".to_string()])
                .collect::<Vec<_>>(),
        ),
        0..=8,
    )
}

proptest! {
    /// `compute_role_rank` equals the max rank across the supplied roles
    /// (with `0` for unregistered roles) and is `0` for an empty role list.
    #[test]
    fn compute_role_rank_returns_max_with_unknown_as_zero(
        ranks in rank_table_strategy(),
        roles in role_list_strategy(),
    ) {
        let computed = compute_role_rank(&roles, |r| ranks.get(r).copied());

        let expected = roles
            .iter()
            .map(|r| ranks.get(r).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);

        prop_assert_eq!(computed, expected);
    }

    /// `compute_role_rank` is non-negative: no combination of inputs can
    /// produce a negative rank. (Unknown roles map to `0` and the rank
    /// table is constrained to positive values.)
    #[test]
    fn compute_role_rank_is_non_negative(
        ranks in rank_table_strategy(),
        roles in role_list_strategy(),
    ) {
        let computed = compute_role_rank(&roles, |r| ranks.get(r).copied());
        prop_assert!(computed >= 0, "compute_role_rank produced negative {computed}");
    }

    /// Adding a known role with a higher rank can only raise (or hold) the
    /// computed rank — never lower it. This is the monotonicity property
    /// the user-mgmt no-upward-visibility rule depends on.
    #[test]
    fn compute_role_rank_is_monotonic_in_role_set(
        ranks in rank_table_strategy(),
        roles in role_list_strategy(),
        extra_idx in 0..ROLE_NAME_POOL.len(),
    ) {
        let base = compute_role_rank(&roles, |r| ranks.get(r).copied());
        let mut extended = roles.clone();
        extended.push(ROLE_NAME_POOL[extra_idx].to_string());
        let bigger = compute_role_rank(&extended, |r| ranks.get(r).copied());
        prop_assert!(
            bigger >= base,
            "adding a role lowered the rank: {base} -> {bigger}"
        );
    }

    /// `platform_admin` (rank `i64::MAX`) dominates: if it appears in the
    /// role list and the resolver returns `i64::MAX` for it, the result
    /// is `i64::MAX` regardless of any other roles.
    #[test]
    fn compute_role_rank_platform_admin_dominates(
        ranks in rank_table_strategy(),
        mut roles in role_list_strategy(),
    ) {
        roles.push("platform_admin".to_string());
        let resolver = |r: &str| {
            if r == "platform_admin" {
                Some(i64::MAX)
            } else {
                ranks.get(r).copied()
            }
        };
        let computed = compute_role_rank(&roles, resolver);
        prop_assert_eq!(computed, i64::MAX);
    }
}
