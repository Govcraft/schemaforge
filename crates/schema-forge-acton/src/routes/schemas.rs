use std::time::Duration;

use acton_service::middleware::Claims;
use acton_service::prelude::ActorHandleInterface;
use acton_service::state::AppState;
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
use tokio::sync::oneshot;
use tracing::instrument;

use crate::access::{
    check_schema_access, schema_permissions, AccessAction, OptionalClaims, SchemaPermissions,
    PLATFORM_ADMIN_ROLE,
};
use crate::actor::ForgeActor;
use crate::config::SchemaForgeConfig;
use crate::error::ForgeError;
use crate::messages::{
    ApplyMigration, GetSchema, InsertSchema, ListSchemas, RemoveSchema, ReplyChannel,
    StoreSchemaMetadata,
};

// ---------------------------------------------------------------------------
// Actor request helper
// ---------------------------------------------------------------------------

/// Timeout for actor request-response round-trips.
const ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

/// Re-run the inverse-relation pairing pass over the full registry with a
/// new or updated schema definition, writing the paired version back into
/// `target`. Isolated in its own function so both `create_schema` and
/// `update_schema` share the exact same logic.
///
/// Note: this only updates `target`. If adding/updating `target` would
/// cause an *existing* schema's stored `-> X[]` field to become derived
/// (because the new schema provides the inverse FK), that existing schema
/// is not rewritten here — the change takes effect on the next daemon
/// restart, which re-pairs the entire registry in `build_init`.
async fn pair_with_registry(
    forge: &acton_service::prelude::ActorHandle,
    target: &mut SchemaDefinition,
) -> Result<(), ForgeError> {
    let (tx, rx) = oneshot::channel();
    forge
        .send(ListSchemas {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let mut batch = ask_forge(rx).await?;

    // Replace any existing entry with the same name so we pair against
    // the incoming definition — not the stale one.
    batch.retain(|s| s.name.as_str() != target.name.as_str());
    batch.push(target.clone());

    schema_forge_core::inverse_relations::pair_inverse_relations(&mut batch).map_err(|e| {
        ForgeError::ValidationFailed {
            details: vec![e.to_string()],
        }
    })?;

    // The target is always the last element we pushed.
    if let Some(paired) = batch.pop() {
        *target = paired;
    }
    Ok(())
}

/// Dry-run the Cedar policy bundle that would result from inserting (or
/// removing, when `removing` is `true`) `target` into the current registry.
///
/// Surfacing the validation error here turns it into a 400-class response —
/// the caller's request is rejected before any DB migration runs. The actor
/// will recompile and atomically swap on the subsequent `InsertSchema` /
/// `RemoveSchema` regardless; this is purely a fail-closed pre-check.
async fn precheck_policy_bundle(
    state: &AppState<SchemaForgeConfig>,
    forge: &acton_service::prelude::ActorHandle,
    target: &SchemaDefinition,
    removing: bool,
) -> Result<(), ForgeError> {
    let (tx, rx) = oneshot::channel();
    forge
        .send(ListSchemas {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let mut proposed = ask_forge(rx).await?;

    proposed.retain(|s| s.name.as_str() != target.name.as_str());
    if !removing {
        proposed.push(target.clone());
    }

    let policy_store = fetch_policy_store(state).await?;
    let role_ranks = policy_store.current().role_ranks.clone();

    crate::authz::store::PolicyStoreSnapshot::from_schemas(&proposed, None, role_ranks).map_err(
        |e| ForgeError::ValidationFailed {
            details: vec![format!("Cedar policy validation failed for proposed schema: {e}")],
        },
    )?;

    Ok(())
}

/// Fetch the current Cedar [`PolicyStore`] from the actor.
async fn fetch_policy_store(
    state: &AppState<SchemaForgeConfig>,
) -> Result<std::sync::Arc<crate::authz::PolicyStore>, ForgeError> {
    let forge = state.actor::<ForgeActor>().ok_or_else(|| ForgeError::Internal {
        message: "ForgeActor not registered".into(),
    })?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(crate::messages::GetPolicyStore {
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await?.ok_or_else(|| ForgeError::Internal {
        message: "Cedar policy store not initialized — InitForge has not run".into(),
    })
}

/// Await an actor response with a timeout.
async fn ask_forge<T>(rx: oneshot::Receiver<T>) -> Result<T, ForgeError> {
    tokio::time::timeout(ACTOR_TIMEOUT, rx)
        .await
        .map_err(|_| ForgeError::Internal {
            message: "forge actor timeout".into(),
        })?
        .map_err(|_| ForgeError::Internal {
            message: "forge actor unavailable".into(),
        })
}

/// Require authentication. Returns 401 if no Claims present.
fn require_auth(claims: &Option<Claims>) -> Result<&Claims, ForgeError> {
    claims.as_ref().ok_or(ForgeError::Unauthorized {
        message: "authentication required".to_string(),
    })
}

/// Require the `platform_admin` role. Returns 403 if the caller lacks it.
fn require_admin(claims: &Claims) -> Result<(), ForgeError> {
    if claims.has_role(PLATFORM_ADMIN_ROLE) {
        Ok(())
    } else {
        Err(ForgeError::Forbidden {
            message: "schema management requires platform_admin role".to_string(),
        })
    }
}

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
    /// Cedar-derived permission summary for the calling user. Only populated
    /// on read paths (`GET /schemas`, `GET /schemas/{name}`); omitted from
    /// write responses where the caller's authority is implicit in the
    /// request having succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<SchemaPermissions>,
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
    /// The field-level annotations (`@widget`, `@format`, `@field_access`,
    /// `@owner`, `@kanban_column`), serialized as the same tagged-enum shape
    /// emitted by `FieldAnnotation`'s derived `Serialize`.
    pub annotations: Vec<serde_json::Value>,
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

/// Project a `FieldDefinition` to a `FieldResponse`, walking `Composite`
/// and `Array` recursively so every level uses the same string-modifier
/// projection as the top level.
fn field_definition_to_response(field: &FieldDefinition) -> FieldResponse {
    FieldResponse {
        name: field.name.as_str().to_string(),
        field_type: field_type_to_json(&field.field_type),
        modifiers: field.modifiers.iter().map(|m| m.to_string()).collect(),
        annotations: field
            .annotations
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or_default())
            .collect(),
    }
}

/// Serialize a `FieldType` to JSON while re-walking `Composite` and `Array`
/// so nested `FieldDefinition`s get the same string-modifier projection as
/// top-level fields. All non-recursive variants pass through the derived
/// serde impl (which uses `#[serde(tag = "type", content = "data")]`).
fn field_type_to_json(field_type: &FieldType) -> serde_json::Value {
    match field_type {
        FieldType::Composite(sub_fields) => {
            let sub_responses: Vec<FieldResponse> = sub_fields
                .iter()
                .map(field_definition_to_response)
                .collect();
            serde_json::json!({
                "type": "Composite",
                "data": sub_responses,
            })
        }
        FieldType::Array(inner) => {
            serde_json::json!({
                "type": "Array",
                "data": field_type_to_json(inner),
            })
        }
        other => serde_json::to_value(other).unwrap_or_default(),
    }
}

/// Convert a `SchemaDefinition` to a `SchemaResponse`.
fn schema_to_response(schema: &SchemaDefinition) -> SchemaResponse {
    let fields = schema
        .fields
        .iter()
        .map(field_definition_to_response)
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
        permissions: None,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /schemas -- Register a new schema. Requires platform_admin role.
#[instrument(skip_all)]
pub async fn create_schema(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<CreateSchemaRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    if let Err(e) = require_admin(claims) {
        if let Some(logger) = state.audit_logger() {
            logger
                .log_custom(
                    "forge.access.denied",
                    acton_service::audit::AuditSeverity::Warning,
                    Some(serde_json::json!({
                        "schema": &body.name,
                        "action": "write",
                        "user": claims.sub,
                    })),
                )
                .await;
        }
        return Err(e);
    }
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // 1. Validate schema name
    let schema_name = SchemaName::new(&body.name).map_err(|_| ForgeError::InvalidSchemaName {
        name: body.name.clone(),
    })?;

    // 2. Check for conflict in registry via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: schema_name.as_str().to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    if ask_forge(rx).await?.is_some() {
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
    let mut definition = SchemaDefinition::new(
        schema_id,
        schema_name.clone(),
        fields,
        Vec::<Annotation>::new(),
    )
    .map_err(|e| ForgeError::ValidationFailed {
        details: vec![e.to_string()],
    })?;

    // 4a. Run the inverse-relation pairing pass across the full registry so
    // any `-> X[]` field paired with an FK from an existing schema is marked
    // as derived before the migration plan is generated.
    pair_with_registry(forge, &mut definition).await?;

    // 4b. Pre-validate the proposed Cedar bundle BEFORE running any DB
    // migration. The actor will recompile and atomically swap on InsertSchema
    // anyway, but doing the dry-run here means a malformed schema is rejected
    // with a 400 instead of leaving the database in a state the running
    // policy bundle can't reason about.
    precheck_policy_bundle(&state, forge, &definition, false).await?;

    // 5. Generate migration plan
    let plan = DiffEngine::create_new(&definition);

    // 6. Apply migration to backend via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(ApplyMigration {
            schema_name: schema_name.clone(),
            steps: plan.steps,
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await?.map_err(ForgeError::from)?;

    // 7. Store schema metadata in backend via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(StoreSchemaMetadata {
            definition: definition.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await?.map_err(ForgeError::from)?;

    // 8. Update registry cache + recompile Cedar bundle. The actor swap is
    // the source of truth: if the recompile fails here despite the dry-run
    // above, the actor reverts the registry mutation and returns the error.
    let (tx, rx) = oneshot::channel();
    forge
        .send(InsertSchema {
            name: schema_name.as_str().to_string(),
            definition: definition.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx)
        .await?
        .map_err(|err| ForgeError::Internal {
            message: format!("Cedar policy recompile failed during schema insertion: {err}"),
        })?;

    // 9. Rebuild GraphQL schema
    // NOTE: GraphQL rebuild will be re-integrated when the graphql module
    // is migrated to actor-based state access.

    // 10. Audit: schema created
    let field_count = definition.fields.len();
    if let Some(logger) = state.audit_logger() {
        logger
            .log_custom(
                "forge.schema.created",
                acton_service::audit::AuditSeverity::Notice,
                Some(serde_json::json!({
                    "schema_name": definition.name.as_str(),
                    "field_count": field_count,
                    "user": claims.sub,
                })),
            )
            .await;
    }

    // 11. Return 201 Created
    let response = schema_to_response(&definition);
    Ok((StatusCode::CREATED, Json(response)))
}

/// GET /schemas -- List schemas the caller is allowed to see, each annotated
/// with the action permissions that drive the admin UI's affordances.
///
/// Schemas the caller cannot `Read` are omitted entirely so the client never
/// has to filter or 403-probe to learn nav contents — the Cedar bundle is
/// the single source of truth.
#[instrument(skip_all)]
pub async fn list_schemas(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    let (tx, rx) = oneshot::channel();
    forge
        .send(ListSchemas {
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schemas = ask_forge(rx).await?;
    let policy_store = fetch_policy_store(&state).await?;

    let mut responses: Vec<SchemaResponse> = Vec::with_capacity(schemas.len());
    for schema in &schemas {
        // Drop schemas the caller cannot read. `check_schema_access(Read)`
        // returns the same decision the entity-list handler would surface
        // as a 403 — keep them in lockstep so visibility and access stay
        // mutually consistent.
        if check_schema_access(&policy_store, schema, Some(claims), AccessAction::Read).is_err() {
            continue;
        }
        let mut response = schema_to_response(schema);
        response.permissions = Some(schema_permissions(&policy_store, schema, Some(claims)));
        responses.push(response);
    }
    let count = responses.len();
    Ok(Json(ListSchemasResponse {
        schemas: responses,
        count,
    }))
}

/// GET /schemas/{name} -- Get a schema by name. Requires read access.
///
/// Returns 403 instead of leaking schema metadata when the caller lacks
/// `Read` access; mirrors the list endpoint's filtering rule so a denied
/// caller can never use this route to enumerate hidden schemas.
#[instrument(skip_all)]
pub async fn get_schema(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(name): Path<String>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: name.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema = ask_forge(rx)
        .await?
        .ok_or(ForgeError::SchemaNotFound { name })?;

    let policy_store = fetch_policy_store(&state).await?;
    check_schema_access(&policy_store, &schema, Some(claims), AccessAction::Read)?;

    let mut response = schema_to_response(&schema);
    response.permissions = Some(schema_permissions(&policy_store, &schema, Some(claims)));
    Ok(Json(response))
}

/// PUT /schemas/{name} -- Update an existing schema (triggers migration). Requires platform_admin role.
#[instrument(skip_all)]
pub async fn update_schema(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(name): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<CreateSchemaRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    if let Err(e) = require_admin(claims) {
        if let Some(logger) = state.audit_logger() {
            logger
                .log_custom(
                    "forge.access.denied",
                    acton_service::audit::AuditSeverity::Warning,
                    Some(serde_json::json!({
                        "schema": &name,
                        "action": "write",
                        "user": claims.sub,
                    })),
                )
                .await;
        }
        return Err(e);
    }
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // 1. Find existing schema via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: name.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let old_schema = ask_forge(rx)
        .await?
        .ok_or(ForgeError::SchemaNotFound { name: name.clone() })?;

    // 2. Validate the updated schema name matches the path
    let schema_name = SchemaName::new(&body.name).map_err(|_| ForgeError::InvalidSchemaName {
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
    let mut new_definition = SchemaDefinition::new(
        old_schema.id.clone(),
        schema_name.clone(),
        fields,
        Vec::<Annotation>::new(),
    )
    .map_err(|e| ForgeError::ValidationFailed {
        details: vec![e.to_string()],
    })?;

    // 4a. Run the inverse-relation pairing pass before diffing, so newly
    // added `-> X[]` fields are classified as derived (and therefore
    // produce no AddRelation step for a physical column).
    pair_with_registry(forge, &mut new_definition).await?;

    // 4b. Dry-run the Cedar bundle for the proposed registry state so an
    // invalid schema fails fast — before any DB migration.
    precheck_policy_bundle(&state, forge, &new_definition, false).await?;

    // 5. Compute diff and generate migration plan
    let plan = DiffEngine::diff(&old_schema, &new_definition);

    // 6. Apply migration steps via actor
    let step_count = plan.steps.len();
    if !plan.is_empty() {
        let (tx, rx) = oneshot::channel();
        forge
            .send(ApplyMigration {
                schema_name: schema_name.clone(),
                steps: plan.steps,
                reply: ReplyChannel::new(tx),
            })
            .await;
        ask_forge(rx).await?.map_err(ForgeError::from)?;
    }

    // 7. Store updated metadata via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(StoreSchemaMetadata {
            definition: new_definition.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx).await?.map_err(ForgeError::from)?;

    // 8. Update registry cache + recompile Cedar bundle.
    let (tx, rx) = oneshot::channel();
    forge
        .send(InsertSchema {
            name: schema_name.as_str().to_string(),
            definition: new_definition.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx)
        .await?
        .map_err(|err| ForgeError::Internal {
            message: format!("Cedar policy recompile failed during schema update: {err}"),
        })?;

    // 9. Rebuild GraphQL schema
    // NOTE: GraphQL rebuild will be re-integrated when the graphql module
    // is migrated to actor-based state access.

    // 10. Audit: schema migrated
    if let Some(logger) = state.audit_logger() {
        logger
            .log_custom(
                "forge.schema.migrated",
                acton_service::audit::AuditSeverity::Notice,
                Some(serde_json::json!({
                    "schema_name": new_definition.name.as_str(),
                    "step_count": step_count,
                    "user": claims.sub,
                })),
            )
            .await;
    }

    Ok(Json(schema_to_response(&new_definition)))
}

/// DELETE /schemas/{name} -- Remove a schema. Requires platform_admin role.
#[instrument(skip_all)]
pub async fn delete_schema(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(name): Path<String>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    if let Err(e) = require_admin(claims) {
        if let Some(logger) = state.audit_logger() {
            logger
                .log_custom(
                    "forge.access.denied",
                    acton_service::audit::AuditSeverity::Warning,
                    Some(serde_json::json!({
                        "schema": &name,
                        "action": "delete",
                        "user": claims.sub,
                    })),
                )
                .await;
        }
        return Err(e);
    }
    let forge = state
        .actor::<ForgeActor>()
        .expect("ForgeActor not registered");

    // 1. Verify schema exists via actor
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: name.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let _schema = ask_forge(rx)
        .await?
        .ok_or(ForgeError::SchemaNotFound { name: name.clone() })?;

    // 2. Remove from registry cache + recompile Cedar bundle. The actor
    // reverts the registry mutation if the recompile fails so the running
    // bundle and registry never drift.
    let (tx, rx) = oneshot::channel();
    forge
        .send(RemoveSchema {
            name: name.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    ask_forge(rx)
        .await?
        .map_err(|err| ForgeError::Internal {
            message: format!("Cedar policy recompile failed during schema deletion: {err}"),
        })?;

    // 3. Rebuild GraphQL schema
    // NOTE: GraphQL rebuild will be re-integrated when the graphql module
    // is migrated to actor-based state access.

    // Note: In a full implementation, we would also drop the backend table.
    // For now, we just remove the metadata and cache entry.

    // 4. Audit: schema deleted
    if let Some(logger) = state.audit_logger() {
        logger
            .log_custom(
                "forge.schema.deleted",
                acton_service::audit::AuditSeverity::Warning,
                Some(serde_json::json!({
                    "schema_name": name,
                    "user": claims.sub,
                })),
            )
            .await;
    }

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
        let result = parse_field_type(&serde_json::json!({"type": "Text", "data": {}})).unwrap();
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
        assert!(response.fields[1]
            .modifiers
            .contains(&"required".to_string()));
    }

    #[test]
    fn schema_to_response_top_level_required_is_string_modifiers() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            )],
            vec![],
        )
        .unwrap();

        let response = schema_to_response(&schema);
        assert_eq!(response.fields[0].modifiers, vec!["required".to_string()]);
        // field_type is the derived tagged shape for primitives
        assert_eq!(
            response.fields[0]
                .field_type
                .get("type")
                .and_then(|v| v.as_str()),
            Some("Text")
        );
    }

    #[test]
    fn schema_to_response_composite_sub_field_modifiers_are_strings() {
        let composite = FieldType::Composite(vec![
            FieldDefinition::new(
                FieldName::new("street").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_modifiers(
                FieldName::new("city").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            ),
        ]);

        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Company").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("headquarters").unwrap(),
                composite,
            )],
            vec![],
        )
        .unwrap();

        let response = schema_to_response(&schema);
        let hq = &response.fields[0];
        assert_eq!(
            hq.field_type.get("type").and_then(|v| v.as_str()),
            Some("Composite")
        );
        let sub_fields = hq
            .field_type
            .get("data")
            .and_then(|v| v.as_array())
            .expect("composite data must be an array");
        assert_eq!(sub_fields.len(), 2);

        // street: empty modifiers as []
        let street_mods = sub_fields[0]
            .get("modifiers")
            .and_then(|v| v.as_array())
            .expect("modifiers must be an array");
        assert!(street_mods.is_empty());

        // city: must be ["required"] (string), never [{"modifier": "Required"}]
        let city_mods = sub_fields[1]
            .get("modifiers")
            .and_then(|v| v.as_array())
            .expect("modifiers must be an array");
        assert_eq!(city_mods.len(), 1);
        assert_eq!(
            city_mods[0].as_str(),
            Some("required"),
            "composite sub-field modifiers must be lowercase strings, not tagged objects; got {city_mods:?}"
        );

        // and sub_fields[1] keys are the same as top-level FieldResponse
        assert_eq!(
            sub_fields[1].get("name").and_then(|v| v.as_str()),
            Some("city")
        );
        assert!(sub_fields[1].get("field_type").is_some());
    }

    #[test]
    fn schema_to_response_array_of_text_walks_through_helper() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Post").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("tags").unwrap(),
                FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            )],
            vec![],
        )
        .unwrap();

        let response = schema_to_response(&schema);
        let tags = &response.fields[0];
        assert_eq!(
            tags.field_type.get("type").and_then(|v| v.as_str()),
            Some("Array")
        );
        let inner = tags
            .field_type
            .get("data")
            .expect("array data must be present");
        assert_eq!(inner.get("type").and_then(|v| v.as_str()), Some("Text"));
    }

    #[test]
    fn field_definition_to_response_serializes_widget_and_format_annotations() {
        use schema_forge_core::types::{FieldAnnotation, FormatType, WidgetType};

        let field = FieldDefinition::with_annotations(
            FieldName::new("salary").unwrap(),
            FieldType::Float(schema_forge_core::types::FloatConstraints::unconstrained()),
            vec![],
            vec![
                FieldAnnotation::Widget {
                    widget_type: WidgetType::Progress,
                },
                FieldAnnotation::Format {
                    format_type: FormatType::Currency,
                },
            ],
        );

        let response = field_definition_to_response(&field);
        assert_eq!(response.annotations.len(), 2);

        let widget = response
            .annotations
            .iter()
            .find(|v| v.get("annotation").and_then(|t| t.as_str()) == Some("Widget"))
            .expect("widget annotation must be present");
        assert_eq!(
            widget.get("widget_type").and_then(|v| v.as_str()),
            Some("progress"),
        );

        let format = response
            .annotations
            .iter()
            .find(|v| v.get("annotation").and_then(|t| t.as_str()) == Some("Format"))
            .expect("format annotation must be present");
        assert_eq!(
            format.get("format_type").and_then(|v| v.as_str()),
            Some("currency"),
        );
    }

    #[test]
    fn schema_to_response_composite_sub_field_annotations_walk_through_helper() {
        use schema_forge_core::types::{FieldAnnotation, WidgetType};

        let composite = FieldType::Composite(vec![
            FieldDefinition::new(
                FieldName::new("street").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::with_annotations(
                FieldName::new("country").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![],
                vec![FieldAnnotation::Widget {
                    widget_type: WidgetType::Tags,
                }],
            ),
        ]);

        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Company").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("headquarters").unwrap(),
                composite,
            )],
            vec![],
        )
        .unwrap();

        let response = schema_to_response(&schema);
        let hq = &response.fields[0];
        let sub_fields = hq
            .field_type
            .get("data")
            .and_then(|v| v.as_array())
            .expect("composite data must be an array");

        // street has no annotations
        let street_anns = sub_fields[0]
            .get("annotations")
            .and_then(|v| v.as_array())
            .expect("annotations must be present (even if empty) on composite sub-fields");
        assert!(street_anns.is_empty());

        // country carries its widget annotation through the recursive walker
        let country_anns = sub_fields[1]
            .get("annotations")
            .and_then(|v| v.as_array())
            .expect("annotations must be present on composite sub-fields");
        assert_eq!(country_anns.len(), 1);
        assert_eq!(
            country_anns[0].get("annotation").and_then(|v| v.as_str()),
            Some("Widget"),
        );
        assert_eq!(
            country_anns[0].get("widget_type").and_then(|v| v.as_str()),
            Some("tags"),
        );
    }

    #[test]
    fn schema_to_response_array_of_composite_recurses_modifiers_to_strings() {
        let inner_composite = FieldType::Composite(vec![FieldDefinition::with_modifiers(
            FieldName::new("label").unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
        )]);

        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Post").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("categories").unwrap(),
                FieldType::Array(Box::new(inner_composite)),
            )],
            vec![],
        )
        .unwrap();

        let response = schema_to_response(&schema);
        let cats = &response.fields[0];
        let inner = cats.field_type.get("data").unwrap();
        assert_eq!(
            inner.get("type").and_then(|v| v.as_str()),
            Some("Composite")
        );
        let sub = inner.get("data").and_then(|v| v.as_array()).unwrap();
        let mods = sub[0].get("modifiers").and_then(|v| v.as_array()).unwrap();
        assert_eq!(mods[0].as_str(), Some("required"));
    }
}
