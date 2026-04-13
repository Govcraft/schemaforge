use std::collections::{HashMap, HashSet};

use schema_forge_backend::entity::Entity;
use schema_forge_core::query::{FieldPath, Filter};
use schema_forge_core::types::{Annotation, DynamicValue, FieldType, SchemaDefinition};

use crate::state::ForgeState;

/// Resolve relation display values for a set of entities.
///
/// Scans the schema for relation fields, collects referenced entity IDs,
/// and batch-fetches referenced entities per target schema in a single
/// `WHERE id IN (...)` query (instead of one query per referenced entity).
pub async fn resolve_ref_display(
    state: &ForgeState,
    schema: &SchemaDefinition,
    entities: &[Entity],
) -> HashMap<String, String> {
    let mut ref_display = HashMap::new();

    // Collect (target_schema_name, {unique_entity_ids}) for each relation field
    let mut targets: HashMap<String, HashSet<String>> = HashMap::new();

    for field in &schema.fields {
        let target_name = match &field.field_type {
            FieldType::Relation { target, .. } => target.as_str().to_string(),
            _ => continue,
        };

        for entity in entities {
            if let Some(val) = entity.field(field.name.as_str()) {
                match val {
                    DynamicValue::Ref(id) => {
                        targets
                            .entry(target_name.clone())
                            .or_default()
                            .insert(id.as_str().to_string());
                    }
                    DynamicValue::RefArray(ids) => {
                        for id in ids {
                            targets
                                .entry(target_name.clone())
                                .or_default()
                                .insert(id.as_str().to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // For each target schema, batch-fetch all referenced entities in one query
    for (target_name, ids) in &targets {
        if ids.is_empty() {
            continue;
        }

        let target_schema = match state.registry.get(target_name).await {
            Some(s) => s,
            None => continue,
        };

        // Find display field for the target schema
        let display_field = target_schema.annotations.iter().find_map(|a| match a {
            Annotation::Display { field } => Some(field.as_str().to_string()),
            _ => None,
        });

        // Build a single query: SELECT id, <display_field> FROM target WHERE id IN (...)
        let id_values: Vec<DynamicValue> = ids
            .iter()
            .map(|id| DynamicValue::Text(id.clone()))
            .collect();

        let mut query = schema_forge_core::query::Query::new(target_schema.id.clone())
            .with_filter(Filter::in_set(FieldPath::single("id"), id_values));

        // Project only the display field to minimize data transfer
        if let Some(ref df) = display_field {
            query = query.with_projection(vec![df.clone()]);
        }

        let result = match state.backend.query(&query).await {
            Ok(r) => r,
            Err(_) => continue,
        };

        for entity in &result.entities {
            let id_str = entity.id.as_str().to_string();
            let label = resolve_entity_label(entity, &target_schema, display_field.as_deref());
            ref_display.insert(id_str, label);
        }
    }

    ref_display
}

/// Resolve a human-readable label for an entity, using the display field
/// annotation or falling back to the first text field.
fn resolve_entity_label(
    entity: &Entity,
    schema: &SchemaDefinition,
    display_field: Option<&str>,
) -> String {
    let id_str = entity.id.as_str().to_string();

    if let Some(df) = display_field {
        return entity
            .field(df)
            .map(|v| match v {
                DynamicValue::Text(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| id_str.clone());
    }

    // Fallback: first text field
    schema
        .fields
        .iter()
        .find_map(|f| {
            if matches!(f.field_type, FieldType::Text(_)) {
                entity.field(f.name.as_str()).map(|v| match v {
                    DynamicValue::Text(s) => s.clone(),
                    other => other.to_string(),
                })
            } else {
                None
            }
        })
        .unwrap_or(id_str)
}
