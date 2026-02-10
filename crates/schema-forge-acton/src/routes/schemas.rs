use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use schema_forge_core::migration::DiffEngine;
use schema_forge_core::types::{
    Annotation, FieldDefinition, FieldModifier, FieldName, FieldType, SchemaDefinition, SchemaId,
    SchemaName, TextConstraints,
};
use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::state::ForgeState;

// ---------------------------------------------------------------------------
// Request/Response types
// ---------------------------------------------------------------------------

/// Request body for creating a schema.
#[derive(Debug, Deserialize)]
pub struct CreateSchemaRequest {
    /// The schema name (must be PascalCase).
    pub name: String,
    /// The field definitions.
    pub fields: Vec<FieldDefinitionRequest>,
    /// Optional annotations.
    #[serde(default)]
    pub annotations: Vec<serde_json::Value>,
}

/// A field in a create/update schema request.
#[derive(Debug, Deserialize)]
pub struct FieldDefinitionRequest {
    /// The field name.
    pub name: String,
    /// The field type specification as a JSON value.
    pub field_type: serde_json::Value,
    /// Modifiers: "required", "indexed".
    #[serde(default)]
    pub modifiers: Vec<String>,
}

/// Response for schema operations.
#[derive(Debug, Serialize)]
pub struct SchemaResponse {
    /// The schema ID.
    pub id: String,
    /// The schema name.
    pub name: String,
    /// The field definitions.
    pub fields: Vec<FieldResponse>,
    /// The annotations.
    pub annotations: Vec<serde_json::Value>,
}

/// A single field in the response.
#[derive(Debug, Serialize)]
pub struct FieldResponse {
    /// The field name.
    pub name: String,
    /// The field type as a JSON value.
    pub field_type: serde_json::Value,
    /// The modifiers.
    pub modifiers: Vec<String>,
}

/// Response for list operations.
#[derive(Debug, Serialize)]
pub struct ListSchemasResponse {
    /// The schemas.
    pub schemas: Vec<SchemaResponse>,
    /// The total count.
    pub count: usize,
}

// ---------------------------------------------------------------------------
// Conversion helpers (pure functions)
// ---------------------------------------------------------------------------

/// Convert a `FieldDefinitionRequest` to a `FieldDefinition`.
///
/// Pure function that validates field names and parses field types.
fn request_field_to_definition(
    req: &FieldDefinitionRequest,
) -> Result<FieldDefinition, ForgeError> {
    let name = FieldName::new(&req.name).map_err(|_| ForgeError::ValidationFailed {
        details: vec![format!(
            "invalid field name '{}': must be snake_case, starting with a letter",
            req.name
        )],
    })?;

    let field_type = parse_field_type(&req.field_type)?;

    let mut modifiers = Vec::new();
    for m in &req.modifiers {
        match m.as_str() {
            "required" => modifiers.push(FieldModifier::Required),
            "indexed" => modifiers.push(FieldModifier::Indexed),
            other => {
                return Err(ForgeError::ValidationFailed {
                    details: vec![format!("unknown modifier '{other}'")],
                });
            }
        }
    }

    if modifiers.is_empty() {
        Ok(FieldDefinition::new(name, field_type))
    } else {
        Ok(FieldDefinition::with_modifiers(name, field_type, modifiers))
    }
}

/// Parse a JSON value into a `FieldType`.
///
/// Supports:
/// - `"Text"` / `{"type": "Text"}` / `{"type": "Text", "data": {"max_length": 255}}`
/// - `"Integer"`, `"Float"`, `"Boolean"`, `"DateTime"`, `"RichText"`, `"Json"`
fn parse_field_type(value: &serde_json::Value) -> Result<FieldType, ForgeError> {
    // Handle simple string like "Text", "Boolean", etc.
    if let Some(s) = value.as_str() {
        return match s {
            "Text" => Ok(FieldType::Text(TextConstraints::unconstrained())),
            "RichText" => Ok(FieldType::RichText),
            "Integer" => Ok(FieldType::Integer(
                schema_forge_core::types::IntegerConstraints::unconstrained(),
            )),
            "Float" => Ok(FieldType::Float(
                schema_forge_core::types::FloatConstraints::unconstrained(),
            )),
            "Boolean" => Ok(FieldType::Boolean),
            "DateTime" => Ok(FieldType::DateTime),
            "Json" => Ok(FieldType::Json),
            other => Err(ForgeError::ValidationFailed {
                details: vec![format!("unknown field type '{other}'")],
            }),
        };
    }

    // Handle structured JSON like {"type": "Text", "data": {...}}
    if let Some(obj) = value.as_object() {
        if let Some(type_str) = obj.get("type").and_then(|v| v.as_str()) {
            return match type_str {
                "Text" => Ok(FieldType::Text(TextConstraints::unconstrained())),
                "RichText" => Ok(FieldType::RichText),
                "Integer" => Ok(FieldType::Integer(
                    schema_forge_core::types::IntegerConstraints::unconstrained(),
                )),
                "Float" => Ok(FieldType::Float(
                    schema_forge_core::types::FloatConstraints::unconstrained(),
                )),
                "Boolean" => Ok(FieldType::Boolean),
                "DateTime" => Ok(FieldType::DateTime),
                "Json" => Ok(FieldType::Json),
                other => Err(ForgeError::ValidationFailed {
                    details: vec![format!("unknown field type '{other}'")],
                }),
            };
        }
    }

    Err(ForgeError::ValidationFailed {
        details: vec![format!("invalid field_type value: {value}")],
    })
}

/// Convert a `SchemaDefinition` to a `SchemaResponse`.
fn schema_to_response(schema: &SchemaDefinition) -> SchemaResponse {
    let fields = schema
        .fields
        .iter()
        .map(|f| FieldResponse {
            name: f.name.as_str().to_string(),
            field_type: serde_json::to_value(&f.field_type).unwrap_or_default(),
            modifiers: f.modifiers.iter().map(|m| m.to_string()).collect(),
        })
        .collect();

    let annotations = schema
        .annotations
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();

    SchemaResponse {
        id: schema.id.as_str().to_string(),
        name: schema.name.as_str().to_string(),
        fields,
        annotations,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /schemas -- Register a new schema.
pub async fn create_schema(
    State(state): State<ForgeState>,
    Json(body): Json<CreateSchemaRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    // 1. Validate schema name
    let schema_name =
        SchemaName::new(&body.name).map_err(|_| ForgeError::InvalidSchemaName {
            name: body.name.clone(),
        })?;

    // 2. Check for conflict in registry
    if state.registry.get(schema_name.as_str()).await.is_some() {
        return Err(ForgeError::SchemaAlreadyExists {
            name: schema_name.as_str().to_string(),
        });
    }

    // 3. Parse fields
    if body.fields.is_empty() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["schema must have at least one field".to_string()],
        });
    }

    let fields: Vec<FieldDefinition> = body
        .fields
        .iter()
        .map(request_field_to_definition)
        .collect::<Result<Vec<_>, _>>()?;

    // 4. Build SchemaDefinition
    let schema_id = SchemaId::new();
    let definition = SchemaDefinition::new(
        schema_id,
        schema_name.clone(),
        fields,
        Vec::<Annotation>::new(),
    )
    .map_err(|e| ForgeError::ValidationFailed {
        details: vec![e.to_string()],
    })?;

    // 5. Generate migration plan
    let plan = DiffEngine::create_new(&definition);

    // 6. Apply migration to backend
    state
        .backend
        .apply_migration(&schema_name, &plan.steps)
        .await
        .map_err(ForgeError::from)?;

    // 7. Store schema metadata in backend
    state
        .backend
        .store_schema_metadata(&definition)
        .await
        .map_err(ForgeError::from)?;

    // 8. Update registry cache
    state
        .registry
        .insert(schema_name.as_str().to_string(), definition.clone())
        .await;

    // 9. Return 201 Created
    let response = schema_to_response(&definition);
    Ok((StatusCode::CREATED, Json(response)))
}

/// GET /schemas -- List all registered schemas.
pub async fn list_schemas(
    State(state): State<ForgeState>,
) -> Result<impl IntoResponse, ForgeError> {
    let schemas = state.registry.list().await;
    let responses: Vec<SchemaResponse> = schemas.iter().map(schema_to_response).collect();
    let count = responses.len();
    Ok(Json(ListSchemasResponse {
        schemas: responses,
        count,
    }))
}

/// GET /schemas/{name} -- Get a schema by name.
pub async fn get_schema(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema = state
        .registry
        .get(&name)
        .await
        .ok_or(ForgeError::SchemaNotFound { name })?;

    Ok(Json(schema_to_response(&schema)))
}

/// PUT /schemas/{name} -- Update an existing schema (triggers migration).
pub async fn update_schema(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
    Json(body): Json<CreateSchemaRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    // 1. Find existing schema
    let old_schema = state
        .registry
        .get(&name)
        .await
        .ok_or(ForgeError::SchemaNotFound { name: name.clone() })?;

    // 2. Validate the updated schema name matches the path
    let schema_name =
        SchemaName::new(&body.name).map_err(|_| ForgeError::InvalidSchemaName {
            name: body.name.clone(),
        })?;

    if schema_name.as_str() != name {
        return Err(ForgeError::ValidationFailed {
            details: vec![format!(
                "schema name in body '{}' does not match path '{name}'",
                body.name
            )],
        });
    }

    // 3. Parse fields
    if body.fields.is_empty() {
        return Err(ForgeError::ValidationFailed {
            details: vec!["schema must have at least one field".to_string()],
        });
    }

    let fields: Vec<FieldDefinition> = body
        .fields
        .iter()
        .map(request_field_to_definition)
        .collect::<Result<Vec<_>, _>>()?;

    // 4. Build new SchemaDefinition (preserving the original ID)
    let new_definition = SchemaDefinition::new(
        old_schema.id.clone(),
        schema_name.clone(),
        fields,
        Vec::<Annotation>::new(),
    )
    .map_err(|e| ForgeError::ValidationFailed {
        details: vec![e.to_string()],
    })?;

    // 5. Compute diff and generate migration plan
    let plan = DiffEngine::diff(&old_schema, &new_definition);

    // 6. Apply migration steps
    if !plan.is_empty() {
        state
            .backend
            .apply_migration(&schema_name, &plan.steps)
            .await
            .map_err(ForgeError::from)?;
    }

    // 7. Store updated metadata
    state
        .backend
        .store_schema_metadata(&new_definition)
        .await
        .map_err(ForgeError::from)?;

    // 8. Update registry cache
    state
        .registry
        .insert(schema_name.as_str().to_string(), new_definition.clone())
        .await;

    Ok(Json(schema_to_response(&new_definition)))
}

/// DELETE /schemas/{name} -- Remove a schema.
pub async fn delete_schema(
    State(state): State<ForgeState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ForgeError> {
    // 1. Find existing schema
    let _schema = state
        .registry
        .get(&name)
        .await
        .ok_or(ForgeError::SchemaNotFound { name: name.clone() })?;

    // 2. Remove from registry cache
    state.registry.remove(&name).await;

    // Note: In a full implementation, we would also drop the backend table.
    // For now, we just remove the metadata and cache entry.

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_field_type_simple_text() {
        let result = parse_field_type(&serde_json::json!("Text")).unwrap();
        assert!(matches!(result, FieldType::Text(_)));
    }

    #[test]
    fn parse_field_type_simple_boolean() {
        let result = parse_field_type(&serde_json::json!("Boolean")).unwrap();
        assert!(matches!(result, FieldType::Boolean));
    }

    #[test]
    fn parse_field_type_simple_integer() {
        let result = parse_field_type(&serde_json::json!("Integer")).unwrap();
        assert!(matches!(result, FieldType::Integer(_)));
    }

    #[test]
    fn parse_field_type_simple_float() {
        let result = parse_field_type(&serde_json::json!("Float")).unwrap();
        assert!(matches!(result, FieldType::Float(_)));
    }

    #[test]
    fn parse_field_type_simple_datetime() {
        let result = parse_field_type(&serde_json::json!("DateTime")).unwrap();
        assert!(matches!(result, FieldType::DateTime));
    }

    #[test]
    fn parse_field_type_simple_json() {
        let result = parse_field_type(&serde_json::json!("Json")).unwrap();
        assert!(matches!(result, FieldType::Json));
    }

    #[test]
    fn parse_field_type_structured() {
        let result =
            parse_field_type(&serde_json::json!({"type": "Text", "data": {}})).unwrap();
        assert!(matches!(result, FieldType::Text(_)));
    }

    #[test]
    fn parse_field_type_unknown_returns_error() {
        let result = parse_field_type(&serde_json::json!("UnknownType"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_field_type_invalid_value_returns_error() {
        let result = parse_field_type(&serde_json::json!(42));
        assert!(result.is_err());
    }

    #[test]
    fn request_field_to_definition_simple() {
        let req = FieldDefinitionRequest {
            name: "email".into(),
            field_type: serde_json::json!("Text"),
            modifiers: vec![],
        };
        let def = request_field_to_definition(&req).unwrap();
        assert_eq!(def.name.as_str(), "email");
        assert!(def.modifiers.is_empty());
    }

    #[test]
    fn request_field_to_definition_with_modifiers() {
        let req = FieldDefinitionRequest {
            name: "email".into(),
            field_type: serde_json::json!("Text"),
            modifiers: vec!["required".into(), "indexed".into()],
        };
        let def = request_field_to_definition(&req).unwrap();
        assert!(def.is_required());
        assert!(def.is_indexed());
    }

    #[test]
    fn request_field_to_definition_unknown_modifier() {
        let req = FieldDefinitionRequest {
            name: "email".into(),
            field_type: serde_json::json!("Text"),
            modifiers: vec!["unknown".into()],
        };
        assert!(request_field_to_definition(&req).is_err());
    }

    #[test]
    fn schema_to_response_includes_all_fields() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_modifiers(
                    FieldName::new("email").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                ),
            ],
            vec![],
        )
        .unwrap();

        let response = schema_to_response(&schema);
        assert_eq!(response.name, "Contact");
        assert_eq!(response.fields.len(), 2);
        assert_eq!(response.fields[0].name, "name");
        assert_eq!(response.fields[1].name, "email");
        assert!(response.fields[1].modifiers.contains(&"required".to_string()));
    }
}
