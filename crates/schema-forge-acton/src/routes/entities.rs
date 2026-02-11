use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use schema_forge_backend::entity::Entity;
use schema_forge_core::types::{
    Cardinality, DynamicValue, EntityId, FieldType, SchemaDefinition, SchemaName,
};
use serde::{Deserialize, Serialize};

use crate::access::{
    check_schema_access, filter_entity_fields, inject_tenant_on_create, inject_tenant_scope,
    AccessAction, FieldFilterDirection, OptionalAuth,
};
use crate::error::ForgeError;
use crate::state::ForgeState;

// ---------------------------------------------------------------------------
// Request/Response types
// ---------------------------------------------------------------------------

/// Request body for creating/updating an entity.
#[derive(Debug, Deserialize)]
pub struct EntityRequest {
    /// The entity fields as a JSON map.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Response for a single entity.
#[derive(Debug, Serialize)]
pub struct EntityResponse {
    /// The entity ID.
    pub id: String,
    /// The schema name.
    pub schema: String,
    /// The entity fields.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Response for entity list/query.
#[derive(Debug, Serialize)]
pub struct ListEntitiesResponse {
    /// The entities.
    pub entities: Vec<EntityResponse>,
    /// The count of entities in this response.
    pub count: usize,
    /// The total count of matching entities before pagination, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<usize>,
}

/// Query parameters for list_entities.
#[derive(Debug, Deserialize, Default)]
pub struct EntityQueryParams {
    /// Maximum number of entities to return.
    pub limit: Option<usize>,
    /// Number of entities to skip.
    pub offset: Option<usize>,
}

// ---------------------------------------------------------------------------
// Conversion helpers (pure functions)
// ---------------------------------------------------------------------------

/// Convert a JSON field map to `DynamicValue` fields using schema type information.
///
/// Pure function: no I/O. Returns a list of validation errors if any fields
/// fail type conversion.
pub fn json_to_entity_fields(
    schema: &SchemaDefinition,
    json_fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<BTreeMap<String, DynamicValue>, Vec<String>> {
    let mut fields = BTreeMap::new();
    let mut errors = Vec::new();

    for (key, value) in json_fields {
        // Look up the field type in the schema for guidance
        let field_def = schema.field(key);

        let dynamic_value = if let Some(def) = field_def {
            convert_json_with_type_hint(value, &def.field_type)
        } else {
            // Unknown field -- convert based on JSON type
            convert_json_untyped(value)
        };

        match dynamic_value {
            Ok(dv) => {
                fields.insert(key.clone(), dv);
            }
            Err(msg) => {
                errors.push(format!("field '{key}': {msg}"));
            }
        }
    }

    // Check for required fields that are missing
    for field_def in &schema.fields {
        if field_def.is_required() && !json_fields.contains_key(field_def.name.as_str()) {
            errors.push(format!(
                "required field '{}' is missing",
                field_def.name.as_str()
            ));
        }
    }

    if errors.is_empty() {
        Ok(fields)
    } else {
        Err(errors)
    }
}

/// Convert a JSON value to a DynamicValue using the field type as a hint.
fn convert_json_with_type_hint(
    value: &serde_json::Value,
    field_type: &FieldType,
) -> Result<DynamicValue, String> {
    match field_type {
        FieldType::Text(_) | FieldType::RichText => match value {
            serde_json::Value::String(s) => Ok(DynamicValue::Text(s.clone())),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            other => Ok(DynamicValue::Text(other.to_string())),
        },
        FieldType::Integer(_) => match value {
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(DynamicValue::Integer)
                .ok_or_else(|| format!("expected integer, got {value}")),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected integer, got {value}")),
        },
        FieldType::Float(_) => match value {
            serde_json::Value::Number(n) => n
                .as_f64()
                .map(DynamicValue::Float)
                .ok_or_else(|| format!("expected float, got {value}")),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected float, got {value}")),
        },
        FieldType::Boolean => match value {
            serde_json::Value::Bool(b) => Ok(DynamicValue::Boolean(*b)),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected boolean, got {value}")),
        },
        FieldType::DateTime => match value {
            serde_json::Value::String(s) => {
                // Try to parse as a DateTime
                s.parse::<chrono::DateTime<chrono::Utc>>()
                    .map(DynamicValue::DateTime)
                    .map_err(|e| format!("invalid datetime '{s}': {e}"))
            }
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected datetime string, got {value}")),
        },
        FieldType::Enum(_) => match value {
            serde_json::Value::String(s) => Ok(DynamicValue::Enum(s.clone())),
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected enum string, got {value}")),
        },
        FieldType::Json => Ok(DynamicValue::Json(value.clone())),
        FieldType::Relation {
            target: _,
            cardinality,
        } => match value {
            serde_json::Value::String(s) => {
                let entity_id = EntityId::parse(s)
                    .map_err(|e| format!("invalid entity reference '{s}': {e}"))?;
                Ok(DynamicValue::Ref(entity_id))
            }
            serde_json::Value::Array(arr) if matches!(cardinality, Cardinality::Many) => {
                let ids = arr
                    .iter()
                    .map(|v| {
                        if let serde_json::Value::String(s) = v {
                            EntityId::parse(s)
                                .map_err(|e| format!("invalid entity reference '{s}': {e}"))
                        } else {
                            Err(format!("expected string entity reference, got {v}"))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DynamicValue::RefArray(ids))
            }
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected entity reference string, got {value}")),
        },
        FieldType::Array(inner) => match value {
            serde_json::Value::Array(arr) => {
                let items = arr
                    .iter()
                    .map(|v| convert_json_with_type_hint(v, inner))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DynamicValue::Array(items))
            }
            serde_json::Value::Null => Ok(DynamicValue::Null),
            _ => Err(format!("expected array, got {value}")),
        },
        _ => convert_json_untyped(value),
    }
}

/// Convert a JSON value to a DynamicValue without type hints.
fn convert_json_untyped(value: &serde_json::Value) -> Result<DynamicValue, String> {
    match value {
        serde_json::Value::Null => Ok(DynamicValue::Null),
        serde_json::Value::Bool(b) => Ok(DynamicValue::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(DynamicValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(DynamicValue::Float(f))
            } else {
                Ok(DynamicValue::Text(n.to_string()))
            }
        }
        serde_json::Value::String(s) => Ok(DynamicValue::Text(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr.iter().map(convert_json_untyped).collect();
            Ok(DynamicValue::Array(items?))
        }
        serde_json::Value::Object(map) => {
            let mut btree = BTreeMap::new();
            for (k, v) in map {
                btree.insert(k.clone(), convert_json_untyped(v)?);
            }
            Ok(DynamicValue::Composite(btree))
        }
    }
}

/// Convert an `Entity` to an `EntityResponse`.
fn entity_to_response(entity: &Entity) -> EntityResponse {
    let mut fields = serde_json::Map::new();
    for (key, value) in &entity.fields {
        fields.insert(key.clone(), dynamic_value_to_json(value));
    }

    EntityResponse {
        id: entity.id.as_str().to_string(),
        schema: entity.schema.as_str().to_string(),
        fields,
    }
}

/// Convert a `DynamicValue` to a JSON value.
fn dynamic_value_to_json(value: &DynamicValue) -> serde_json::Value {
    match value {
        DynamicValue::Null => serde_json::Value::Null,
        DynamicValue::Text(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Integer(i) => serde_json::json!(i),
        DynamicValue::Float(f) => serde_json::json!(f),
        DynamicValue::Boolean(b) => serde_json::Value::Bool(*b),
        DynamicValue::DateTime(dt) => serde_json::Value::String(dt.to_rfc3339()),
        DynamicValue::Enum(s) => serde_json::Value::String(s.clone()),
        DynamicValue::Json(v) => v.clone(),
        DynamicValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(dynamic_value_to_json).collect())
        }
        DynamicValue::Composite(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), dynamic_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        DynamicValue::Ref(id) => serde_json::Value::String(id.as_str().to_string()),
        DynamicValue::RefArray(ids) => serde_json::Value::Array(
            ids.iter()
                .map(|id| serde_json::Value::String(id.as_str().to_string()))
                .collect(),
        ),
        _ => serde_json::Value::Null,
    }
}

/// Validate and parse a schema name from a path parameter.
fn validate_schema_name(name: &str) -> Result<SchemaName, ForgeError> {
    SchemaName::new(name).map_err(|_| ForgeError::InvalidSchemaName {
        name: name.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /schemas/{schema}/entities -- Create a new entity.
pub async fn create_entity(
    State(state): State<ForgeState>,
    Path(schema): Path<String>,
    OptionalAuth(auth): OptionalAuth,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;

    // Look up schema in registry
    let schema_def =
        state
            .registry
            .get(schema_name.as_str())
            .await
            .ok_or(ForgeError::SchemaNotFound {
                name: schema_name.as_str().to_string(),
            })?;

    // Access check
    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)?;

    // Convert JSON fields to DynamicValue fields
    let mut fields = json_to_entity_fields(&schema_def, &body.fields)
        .map_err(|errors| ForgeError::ValidationFailed { details: errors })?;

    // Inject _tenant field from auth context
    inject_tenant_on_create(&mut fields, auth.as_ref(), &state.tenant_config);

    // Create the entity, filtering write-restricted fields
    let mut entity = Entity::new(schema_name, fields);
    filter_entity_fields(
        &mut entity,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Write,
    );

    let mut created = state
        .backend
        .create(&entity)
        .await
        .map_err(ForgeError::from)?;

    // Filter read-restricted fields from response
    filter_entity_fields(
        &mut created,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Read,
    );

    Ok((StatusCode::CREATED, Json(entity_to_response(&created))))
}

/// GET /schemas/{schema}/entities -- List/query entities.
pub async fn list_entities(
    State(state): State<ForgeState>,
    Path(schema): Path<String>,
    OptionalAuth(auth): OptionalAuth,
    Query(params): Query<EntityQueryParams>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;

    // Verify schema exists
    let schema_def =
        state
            .registry
            .get(schema_name.as_str())
            .await
            .ok_or(ForgeError::SchemaNotFound {
                name: schema_name.as_str().to_string(),
            })?;

    // Access check
    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)?;

    // Build a query
    let mut query = schema_forge_core::query::Query::new(schema_def.id.clone());
    if let Some(limit) = params.limit {
        query = query.with_limit(limit);
    }
    if let Some(offset) = params.offset {
        query = query.with_offset(offset);
    }

    // Inject tenant scope filter
    inject_tenant_scope(&mut query, auth.as_ref(), &state.tenant_config);

    let result = state
        .backend
        .query(&query)
        .await
        .map_err(ForgeError::from)?;

    // Record-level access filtering (e.g. @owner)
    let visible_entities =
        if let (Some(ref policy), Some(ref auth_ctx)) = (&state.record_access_policy, &auth) {
            policy
                .filter_visible(&schema_def, auth_ctx, result.entities)
                .await
        } else {
            result.entities
        };

    // Filter read-restricted fields from each entity
    let entities: Vec<EntityResponse> = visible_entities
        .into_iter()
        .map(|mut e| {
            filter_entity_fields(
                &mut e,
                &schema_def,
                auth.as_ref(),
                FieldFilterDirection::Read,
            );
            entity_to_response(&e)
        })
        .collect();
    let count = entities.len();

    Ok(Json(ListEntitiesResponse {
        entities,
        count,
        total_count: result.total_count,
    }))
}

/// GET /schemas/{schema}/entities/{id} -- Get entity by ID.
pub async fn get_entity(
    State(state): State<ForgeState>,
    Path((schema, id)): Path<(String, String)>,
    OptionalAuth(auth): OptionalAuth,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;

    // Look up schema for access check
    let schema_def =
        state
            .registry
            .get(schema_name.as_str())
            .await
            .ok_or(ForgeError::SchemaNotFound {
                name: schema_name.as_str().to_string(),
            })?;

    // Access check
    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Read)?;

    // Parse the entity ID
    let entity_id =
        EntityId::parse(&id).map_err(|_| ForgeError::InvalidEntityId { id: id.clone() })?;

    let mut entity = state
        .backend
        .get(&schema_name, &entity_id)
        .await
        .map_err(ForgeError::from)?;

    // Record-level visibility check
    if let (Some(ref policy), Some(ref auth_ctx)) = (&state.record_access_policy, &auth) {
        let visible = policy
            .filter_visible(&schema_def, auth_ctx, vec![entity.clone()])
            .await;
        if visible.is_empty() {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to view entity '{id}'"),
            });
        }
    }

    // Filter read-restricted fields from response
    filter_entity_fields(
        &mut entity,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Read,
    );

    Ok(Json(entity_to_response(&entity)))
}

/// PUT /schemas/{schema}/entities/{id} -- Update entity.
pub async fn update_entity(
    State(state): State<ForgeState>,
    Path((schema, id)): Path<(String, String)>,
    OptionalAuth(auth): OptionalAuth,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;

    // Parse the entity ID
    let entity_id =
        EntityId::parse(&id).map_err(|_| ForgeError::InvalidEntityId { id: id.clone() })?;

    // Look up schema
    let schema_def =
        state
            .registry
            .get(schema_name.as_str())
            .await
            .ok_or(ForgeError::SchemaNotFound {
                name: schema_name.as_str().to_string(),
            })?;

    // Access check
    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Write)?;

    // Record-level ownership check: fetch existing entity and verify ownership
    if let (Some(ref policy), Some(ref auth_ctx)) = (&state.record_access_policy, &auth) {
        let existing = state
            .backend
            .get(&schema_name, &entity_id)
            .await
            .map_err(ForgeError::from)?;
        if !policy.can_modify(&schema_def, auth_ctx, &existing).await {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to modify entity '{id}'"),
            });
        }
    }

    // Convert JSON fields
    let fields = json_to_entity_fields(&schema_def, &body.fields)
        .map_err(|errors| ForgeError::ValidationFailed { details: errors })?;

    // Build entity with specific ID, filtering write-restricted fields
    let mut entity = Entity::with_id(entity_id, schema_name, fields);
    filter_entity_fields(
        &mut entity,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Write,
    );

    let mut updated = state
        .backend
        .update(&entity)
        .await
        .map_err(ForgeError::from)?;

    // Filter read-restricted fields from response
    filter_entity_fields(
        &mut updated,
        &schema_def,
        auth.as_ref(),
        FieldFilterDirection::Read,
    );

    Ok(Json(entity_to_response(&updated)))
}

/// DELETE /schemas/{schema}/entities/{id} -- Delete entity.
pub async fn delete_entity(
    State(state): State<ForgeState>,
    Path((schema, id)): Path<(String, String)>,
    OptionalAuth(auth): OptionalAuth,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;

    // Look up schema for access check
    let schema_def =
        state
            .registry
            .get(schema_name.as_str())
            .await
            .ok_or(ForgeError::SchemaNotFound {
                name: schema_name.as_str().to_string(),
            })?;

    // Access check
    check_schema_access(&schema_def, auth.as_ref(), AccessAction::Delete)?;

    let entity_id =
        EntityId::parse(&id).map_err(|_| ForgeError::InvalidEntityId { id: id.clone() })?;

    // Record-level ownership check: fetch entity first and verify ownership
    if let (Some(ref policy), Some(ref auth_ctx)) = (&state.record_access_policy, &auth) {
        let entity = state
            .backend
            .get(&schema_name, &entity_id)
            .await
            .map_err(ForgeError::from)?;
        if !policy.can_delete(&schema_def, auth_ctx, &entity).await {
            return Err(ForgeError::Forbidden {
                message: format!("not authorized to delete entity '{id}'"),
            });
        }
    }

    state
        .backend
        .delete(&schema_name, &entity_id)
        .await
        .map_err(ForgeError::from)?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Cardinality, FieldDefinition, FieldModifier, FieldName, SchemaId, TextConstraints,
    };

    fn make_test_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("age").unwrap(),
                    FieldType::Integer(
                        schema_forge_core::types::IntegerConstraints::unconstrained(),
                    ),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn json_to_entity_fields_basic() {
        let schema = make_test_schema();
        let mut json_fields = serde_json::Map::new();
        json_fields.insert("name".into(), serde_json::json!("Alice"));
        json_fields.insert("age".into(), serde_json::json!(30));
        json_fields.insert("active".into(), serde_json::json!(true));

        let result = json_to_entity_fields(&schema, &json_fields).unwrap();
        assert_eq!(
            result.get("name"),
            Some(&DynamicValue::Text("Alice".into()))
        );
        assert_eq!(result.get("age"), Some(&DynamicValue::Integer(30)));
        assert_eq!(result.get("active"), Some(&DynamicValue::Boolean(true)));
    }

    #[test]
    fn json_to_entity_fields_missing_required() {
        let schema = make_test_schema();
        let json_fields = serde_json::Map::new(); // missing "name" which is required

        let result = json_to_entity_fields(&schema, &json_fields);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("required") && e.contains("name")));
    }

    #[test]
    fn json_to_entity_fields_type_mismatch() {
        let schema = make_test_schema();
        let mut json_fields = serde_json::Map::new();
        json_fields.insert("name".into(), serde_json::json!("Alice"));
        json_fields.insert("age".into(), serde_json::json!("not a number"));

        let result = json_to_entity_fields(&schema, &json_fields);
        assert!(result.is_err());
    }

    #[test]
    fn json_to_entity_fields_unknown_field_accepted() {
        let schema = make_test_schema();
        let mut json_fields = serde_json::Map::new();
        json_fields.insert("name".into(), serde_json::json!("Alice"));
        json_fields.insert("extra".into(), serde_json::json!("extra value"));

        let result = json_to_entity_fields(&schema, &json_fields).unwrap();
        assert_eq!(
            result.get("extra"),
            Some(&DynamicValue::Text("extra value".into()))
        );
    }

    #[test]
    fn dynamic_value_to_json_primitives() {
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Null),
            serde_json::Value::Null
        );
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Text("hello".into())),
            serde_json::json!("hello")
        );
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Integer(42)),
            serde_json::json!(42)
        );
        assert_eq!(
            dynamic_value_to_json(&DynamicValue::Boolean(true)),
            serde_json::json!(true)
        );
    }

    #[test]
    fn entity_to_response_roundtrip() {
        let entity = Entity::new(
            SchemaName::new("Contact").unwrap(),
            BTreeMap::from([
                ("name".to_string(), DynamicValue::Text("Alice".into())),
                ("age".to_string(), DynamicValue::Integer(30)),
            ]),
        );
        let response = entity_to_response(&entity);
        assert_eq!(response.schema, "Contact");
        assert!(response.id.starts_with("entity_"));
        assert_eq!(
            response.fields.get("name"),
            Some(&serde_json::json!("Alice"))
        );
        assert_eq!(response.fields.get("age"), Some(&serde_json::json!(30)));
    }

    #[test]
    fn convert_json_untyped_all_types() {
        assert_eq!(
            convert_json_untyped(&serde_json::json!(null)).unwrap(),
            DynamicValue::Null
        );
        assert_eq!(
            convert_json_untyped(&serde_json::json!(true)).unwrap(),
            DynamicValue::Boolean(true)
        );
        assert_eq!(
            convert_json_untyped(&serde_json::json!(42)).unwrap(),
            DynamicValue::Integer(42)
        );
        assert_eq!(
            convert_json_untyped(&serde_json::json!("text")).unwrap(),
            DynamicValue::Text("text".into())
        );
    }

    #[test]
    fn convert_json_untyped_array() {
        let result = convert_json_untyped(&serde_json::json!([1, 2, 3])).unwrap();
        assert!(matches!(result, DynamicValue::Array(arr) if arr.len() == 3));
    }

    #[test]
    fn convert_json_untyped_object() {
        let result = convert_json_untyped(&serde_json::json!({"key": "value"})).unwrap();
        assert!(matches!(result, DynamicValue::Composite(map) if map.len() == 1));
    }

    #[test]
    fn convert_relation_ref_from_json() {
        let entity_id = EntityId::new();
        let id_str = entity_id.as_str().to_string();
        let field_type = FieldType::Relation {
            target: SchemaName::new("Project").unwrap(),
            cardinality: Cardinality::One,
        };
        let result = convert_json_with_type_hint(&serde_json::json!(id_str), &field_type).unwrap();
        assert!(matches!(result, DynamicValue::Ref(ref id) if id.as_str() == id_str));
    }

    #[test]
    fn convert_relation_ref_null() {
        let field_type = FieldType::Relation {
            target: SchemaName::new("Project").unwrap(),
            cardinality: Cardinality::One,
        };
        let result = convert_json_with_type_hint(&serde_json::json!(null), &field_type).unwrap();
        assert_eq!(result, DynamicValue::Null);
    }

    #[test]
    fn convert_relation_ref_array() {
        let id1 = EntityId::new();
        let id2 = EntityId::new();
        let field_type = FieldType::Relation {
            target: SchemaName::new("Tag").unwrap(),
            cardinality: Cardinality::Many,
        };
        let json = serde_json::json!([id1.as_str(), id2.as_str()]);
        let result = convert_json_with_type_hint(&json, &field_type).unwrap();
        assert!(matches!(result, DynamicValue::RefArray(ids) if ids.len() == 2));
    }

    #[test]
    fn convert_array_with_type_hint() {
        let field_type = FieldType::Array(Box::new(FieldType::Boolean));
        let json = serde_json::json!([true, false, true]);
        let result = convert_json_with_type_hint(&json, &field_type).unwrap();
        assert!(matches!(result, DynamicValue::Array(arr) if arr.len() == 3));
    }
}
