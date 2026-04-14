//! Pair parent collection relations (`-> X[]`) with child foreign-key
//! relations (`-> Parent`) to produce derived inverse collections.
//!
//! A derived field has no physical column. It is resolved at read time by
//! querying the child table filtered by the FK, and writes to it are
//! rejected at the API layer. See issue #34 for rationale.

use std::collections::HashMap;
use std::fmt;

use crate::types::{Cardinality, FieldType, SchemaDefinition};

/// Failure modes for the pairing pass.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InverseRelationError {
    /// A parent `-> X[]` field could pair with more than one FK on the
    /// child side. The author must disambiguate by removing duplicate FKs.
    Ambiguous {
        parent_schema: String,
        parent_field: String,
        child_schema: String,
        candidates: Vec<String>,
    },
}

impl fmt::Display for InverseRelationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ambiguous {
                parent_schema,
                parent_field,
                child_schema,
                candidates,
            } => {
                write!(
                    f,
                    "ambiguous inverse relation for {parent_schema}.{parent_field}: \
                     schema {child_schema} has {} fields pointing back at {parent_schema} [{}]",
                    candidates.len(),
                    candidates.join(", "),
                )
            }
        }
    }
}

impl std::error::Error for InverseRelationError {}

/// Scan the batch of schemas and mark each `-> X[]` parent field as derived
/// when the child schema has exactly one `-> Parent` FK pointing back.
///
/// Behavior per parent `-> X[]` field:
/// - **0 matching FKs on X**: leave as stored `TEXT[]` (M2M / tag-style lists
///   still work).
/// - **1 matching FK on X**: set `derived_from = Some(child_fk_field_name)`.
/// - **2+ matching FKs on X**: return [`InverseRelationError::Ambiguous`].
/// - **X not in batch**: leave alone (external or unknown target).
///
/// Idempotent: a field already marked as derived is re-evaluated the same
/// way, so running this multiple times over the same batch is safe.
pub fn pair_inverse_relations(
    schemas: &mut [SchemaDefinition],
) -> Result<(), InverseRelationError> {
    // Build an index: target-schema-name -> list of (owning_schema_index,
    // FK field index, FK field name) for every `-> Parent` (cardinality
    // One) field across the batch. This lets us answer "how many FKs does
    // schema X have pointing at schema P?" in one pass.
    let mut child_fks: HashMap<String, Vec<(usize, String, String)>> = HashMap::new();
    for (schema_idx, schema) in schemas.iter().enumerate() {
        for field in &schema.fields {
            if let FieldType::Relation {
                target,
                cardinality: Cardinality::One,
            } = &field.field_type
            {
                child_fks.entry(target.as_str().to_string()).or_default().push((
                    schema_idx,
                    schema.name.as_str().to_string(),
                    field.name.as_str().to_string(),
                ));
            }
        }
    }

    // Collect the pairings we need to apply. We can't mutate fields while
    // iterating `schemas`, so gather (parent_idx, field_idx, child_fk_name)
    // tuples first and apply them in a second pass.
    let mut pairings: Vec<(usize, usize, String)> = Vec::new();

    for (parent_idx, parent_schema) in schemas.iter().enumerate() {
        let parent_name = parent_schema.name.as_str().to_string();
        for (field_idx, field) in parent_schema.fields.iter().enumerate() {
            let FieldType::Relation {
                target,
                cardinality: Cardinality::Many,
            } = &field.field_type
            else {
                continue;
            };
            let target_name = target.as_str();

            // FKs that exist on ANY schema and point at this parent. We
            // only care about FKs living on the target schema itself.
            let Some(all_fks) = child_fks.get(&parent_name) else {
                continue;
            };
            let matches: Vec<&(usize, String, String)> = all_fks
                .iter()
                .filter(|(_, owner_name, _)| owner_name == target_name)
                .collect();

            match matches.as_slice() {
                [] => {
                    // No FK back — leave as stored TEXT[] (M2M behavior).
                }
                [(_, _, fk_field)] => {
                    pairings.push((parent_idx, field_idx, fk_field.clone()));
                }
                many => {
                    let candidates: Vec<String> =
                        many.iter().map(|(_, _, name)| name.clone()).collect();
                    return Err(InverseRelationError::Ambiguous {
                        parent_schema: parent_name.clone(),
                        parent_field: field.name.as_str().to_string(),
                        child_schema: target_name.to_string(),
                        candidates,
                    });
                }
            }
        }
    }

    for (parent_idx, field_idx, fk_field_name) in pairings {
        // Re-construct FieldName for the child FK. We know it's valid
        // because it already exists on a parsed schema.
        let fk_name = crate::types::FieldName::new(&fk_field_name)
            .expect("child FK field name round-trips through FieldName validation");
        schemas[parent_idx].fields[field_idx].derived_from = Some(fk_name);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        FieldDefinition, FieldModifier, FieldName, FieldType, SchemaDefinition, SchemaId,
        SchemaName, TextConstraints,
    };

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::with_modifiers(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
        )
    }

    fn rel_one(name: &str, target: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Relation {
                target: SchemaName::new(target).unwrap(),
                cardinality: Cardinality::One,
            },
        )
    }

    fn rel_many(name: &str, target: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Relation {
                target: SchemaName::new(target).unwrap(),
                cardinality: Cardinality::Many,
            },
        )
    }

    fn schema(name: &str, fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            fields,
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn paired_one_to_many_marks_derived() {
        let mut schemas = vec![
            schema(
                "Opportunity",
                vec![text("title"), rel_many("documents", "Document")],
            ),
            schema(
                "Document",
                vec![text("title"), rel_one("opportunity", "Opportunity")],
            ),
        ];
        pair_inverse_relations(&mut schemas).unwrap();
        let docs_field = schemas[0].field("documents").unwrap();
        assert_eq!(
            docs_field.derived_from.as_ref().map(|f| f.as_str()),
            Some("opportunity")
        );
        // Child FK field stays stored.
        let parent_fk = schemas[1].field("opportunity").unwrap();
        assert!(parent_fk.derived_from.is_none());
    }

    #[test]
    fn no_fk_back_leaves_stored() {
        let mut schemas = vec![
            schema("Post", vec![text("title"), rel_many("tags", "Tag")]),
            schema("Tag", vec![text("label")]),
        ];
        pair_inverse_relations(&mut schemas).unwrap();
        assert!(schemas[0].field("tags").unwrap().derived_from.is_none());
    }

    #[test]
    fn ambiguous_multiple_fks_errors() {
        let mut schemas = vec![
            schema("Parent", vec![text("name"), rel_many("kids", "Child")]),
            schema(
                "Child",
                vec![
                    text("name"),
                    rel_one("primary_parent", "Parent"),
                    rel_one("secondary_parent", "Parent"),
                ],
            ),
        ];
        let err = pair_inverse_relations(&mut schemas).unwrap_err();
        match err {
            InverseRelationError::Ambiguous {
                parent_schema,
                parent_field,
                child_schema,
                candidates,
            } => {
                assert_eq!(parent_schema, "Parent");
                assert_eq!(parent_field, "kids");
                assert_eq!(child_schema, "Child");
                assert_eq!(candidates.len(), 2);
                assert!(candidates.contains(&"primary_parent".to_string()));
                assert!(candidates.contains(&"secondary_parent".to_string()));
            }
        }
    }

    #[test]
    fn unknown_target_leaves_stored() {
        let mut schemas = vec![schema(
            "Post",
            vec![text("title"), rel_many("mentions", "Missing")],
        )];
        pair_inverse_relations(&mut schemas).unwrap();
        assert!(schemas[0].field("mentions").unwrap().derived_from.is_none());
    }

    #[test]
    fn fk_on_third_schema_does_not_pair() {
        // Document has an FK to Opportunity, but Tag doesn't. So
        // Opportunity.tags[] should NOT pair with anything on Tag.
        let mut schemas = vec![
            schema(
                "Opportunity",
                vec![
                    text("title"),
                    rel_many("documents", "Document"),
                    rel_many("tags", "Tag"),
                ],
            ),
            schema(
                "Document",
                vec![text("title"), rel_one("opportunity", "Opportunity")],
            ),
            schema("Tag", vec![text("label")]),
        ];
        pair_inverse_relations(&mut schemas).unwrap();
        assert_eq!(
            schemas[0]
                .field("documents")
                .unwrap()
                .derived_from
                .as_ref()
                .map(|f| f.as_str()),
            Some("opportunity")
        );
        assert!(schemas[0].field("tags").unwrap().derived_from.is_none());
    }

    #[test]
    fn idempotent() {
        let mut schemas = vec![
            schema(
                "Opportunity",
                vec![text("title"), rel_many("documents", "Document")],
            ),
            schema(
                "Document",
                vec![text("title"), rel_one("opportunity", "Opportunity")],
            ),
        ];
        pair_inverse_relations(&mut schemas).unwrap();
        pair_inverse_relations(&mut schemas).unwrap();
        assert_eq!(
            schemas[0]
                .field("documents")
                .unwrap()
                .derived_from
                .as_ref()
                .map(|f| f.as_str()),
            Some("opportunity")
        );
    }
}
