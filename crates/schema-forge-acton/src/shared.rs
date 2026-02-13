use std::collections::HashMap;

use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Annotation, DynamicValue, EntityId, FieldType, SchemaDefinition, SchemaName,
};

use crate::state::ForgeState;

/// Resolve relation display values for a set of entities.
///
/// Scans the schema for relation fields, collects referenced entity IDs,
/// fetches those entities from the backend, and returns a map from
/// entity ID string → display value string.
pub async fn resolve_ref_display(
    state: &ForgeState,
    schema: &SchemaDefinition,
    entities: &[Entity],
) -> HashMap<String, String> {
    let mut ref_display = HashMap::new();

    // Collect (target_schema_name, [entity_ids]) for each relation field
    let mut targets: HashMap<String, Vec<String>> = HashMap::new();

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
                            .push(id.as_str().to_string());
                    }
                    DynamicValue::RefArray(ids) => {
                        for id in ids {
                            targets
                                .entry(target_name.clone())
                                .or_default()
                                .push(id.as_str().to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // For each target schema, fetch the entities and extract display values
    for (target_name, ids) in &targets {
        let target_schema = match state.registry.get(target_name).await {
            Some(s) => s,
            None => continue,
        };

        // Find display field for the target schema
        let display_field = target_schema.annotations.iter().find_map(|a| match a {
            Annotation::Display { field } => Some(field.as_str().to_string()),
            _ => None,
        });

        for id_str in ids {
            if ref_display.contains_key(id_str) {
                continue; // already resolved
            }
            let entity_id = match EntityId::parse(id_str) {
                Ok(eid) => eid,
                Err(_) => continue,
            };
            let target_sn = match SchemaName::new(target_name) {
                Ok(sn) => sn,
                Err(_) => continue,
            };
            let entity = match state.backend.get(&target_sn, &entity_id).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            let label = resolve_entity_label(&entity, &target_schema, display_field.as_deref());
            ref_display.insert(id_str.clone(), label);
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
