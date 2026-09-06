//! Bounded, currently authorized create reconciliation for hook-free schemas.
use super::entities::{self, EntityRequest};
use crate::{
    access::{check_schema_access, AccessAction, OptionalClaims},
    actor::ForgeActor,
    config::SchemaForgeConfig,
    error::ForgeError,
    messages::{GetSchema, ProcessCreateIntent, ReplyChannel},
};
use acton_service::{middleware::Claims, prelude::ActorHandleInterface, state::AppState};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use schema_forge_backend::{
    create_intent::{
        CreateFingerprint, CreateIntentError, CreateIntentId, CreateIntentReceipt,
        CreateIntentRequest, CreateIntentScope,
    },
    Entity, TenantRef,
};
use schema_forge_core::types::{SchemaDefinition, SchemaName};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};
use tokio::sync::oneshot;

pub(super) const HEADER: &str = "create-intent";
pub(super) fn error(error: CreateIntentError) -> ForgeError {
    let reason = match error {
        CreateIntentError::Unsupported => "create_intent_unsupported",
        CreateIntentError::Invalid => {
            return ForgeError::InvalidQuery {
                message: "Invalid create intent.".into(),
            }
        }
        CreateIntentError::Unavailable => "create_intent_unavailable",
        CreateIntentError::ContentConflict => "create_intent_content_conflict",
        CreateIntentError::SchemaChanged => "create_intent_schema_changed",
        CreateIntentError::Backend(schema_forge_backend::BackendError::UniqueViolation {
            schema,
            field,
        }) => return ForgeError::UniqueViolation { schema, field },
        CreateIntentError::Backend(_) => return ForgeError::BackendUnavailable {
            message:
                "Create outcome could not be determined. Reconcile the same intent before retrying."
                    .into(),
        },
    };
    ForgeError::Conflict {
        reason,
        message:
            "Create intent cannot be used for this request. No replacement create was attempted."
                .into(),
    }
}

/// Sort objects recursively. Arrays, missing/null and JSON number kinds remain distinct.
fn canonical(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(fields) => {
            let sorted: BTreeMap<_, _> = fields
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical).collect())
        }
        _ => value.clone(),
    }
}
fn fingerprint(body: &EntityRequest) -> Result<CreateFingerprint, ForgeError> {
    let bytes = serde_json::to_vec(&canonical(&serde_json::Value::Object(body.fields.clone())))
        .map_err(|_| error(CreateIntentError::Invalid))?;
    CreateFingerprint::parse(hex::encode(Sha256::digest(bytes))).map_err(error)
}

pub(super) async fn process(
    state: &AppState<SchemaForgeConfig>,
    request: CreateIntentRequest,
) -> Result<CreateIntentReceipt, ForgeError> {
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| error(CreateIntentError::Unsupported))?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(ProcessCreateIntent {
            request,
            reply: ReplyChannel::new(tx),
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| ForgeError::BackendUnavailable {
            message: "Create outcome is uncertain. Reconcile this intent before retrying.".into(),
        })?
        .map_err(|_| ForgeError::BackendUnavailable {
            message: "Create outcome is uncertain. Reconcile this intent before retrying.".into(),
        })?
        .map_err(error)
}

async fn scope(
    state: &AppState<SchemaForgeConfig>,
    schema: SchemaDefinition,
    claims: Option<&Claims>,
) -> Result<CreateIntentScope, ForgeError> {
    let store = entities::fetch_export_policy_store(state).await?;
    check_schema_access(&store, &schema, claims, AccessAction::Create)?;
    let caller = claims.ok_or_else(|| ForgeError::Unauthorized {
        message: "Authentication required.".into(),
    })?;
    let tenants: Vec<TenantRef> = caller.custom_claim_as("tenant_chain").unwrap_or_default();
    let tenant = tenants
        .last()
        .map(|tenant| format!("{}:{}", tenant.schema, tenant.entity_id))
        .unwrap_or_default();
    if schema.is_tenanted() && tenant.is_empty() {
        return Err(ForgeError::Forbidden {
            message: "An active tenant is required.".into(),
        });
    }
    Ok(CreateIntentScope {
        principal: crate::authz::adapters::user_id_from_sub(&caller.sub).to_string(),
        tenant,
        schema,
    })
}
fn supported(
    state: &AppState<SchemaForgeConfig>,
    schema: &SchemaDefinition,
) -> Result<(), ForgeError> {
    if schema.has_hooks() || state.config().custom.schema_forge.webhooks.enabled {
        return Err(error(CreateIntentError::Unsupported));
    }
    Ok(())
}
async fn load_scope(
    state: &AppState<SchemaForgeConfig>,
    schema: &str,
    claims: Option<&Claims>,
) -> Result<CreateIntentScope, ForgeError> {
    let name = SchemaName::new(schema).map_err(|_| ForgeError::InvalidSchemaName {
        name: schema.into(),
    })?;
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| error(CreateIntentError::Unsupported))?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: name.to_string(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| error(CreateIntentError::Unsupported))?
        .map_err(|_| error(CreateIntentError::Unsupported))?
        .ok_or(ForgeError::SchemaNotFound {
            name: name.to_string(),
        })?;
    scope(state, schema, claims).await
}
async fn authorize_input(
    state: &AppState<SchemaForgeConfig>,
    scope: &CreateIntentScope,
    claims: Option<&Claims>,
    body: &EntityRequest,
) -> Result<(), ForgeError> {
    entities::reject_hidden_fields_in_body(&scope.schema, &body.fields)?;
    if body
        .fields
        .keys()
        .any(|name| scope.schema.field(name).is_none())
    {
        return Err(ForgeError::InvalidQuery {
            message: "Unknown field in create intent.".into(),
        });
    }
    let mut fields = entities::json_to_entity_fields(&scope.schema, &body.fields)
        .map_err(|details| ForgeError::ValidationFailed { details })?;
    crate::access::inject_owner_on_create(&mut fields, &scope.schema, claims);
    if let Some((_, tenant)) = scope.tenant.split_once(':') {
        fields.insert(
            "_tenant".into(),
            schema_forge_core::types::DynamicValue::Text(tenant.into()),
        );
    }
    let baseline = Entity::new(scope.schema.name.clone(), fields);
    let store = entities::fetch_export_policy_store(state).await?;
    entities::authorize_conditional_fields(&store, &scope.schema, &baseline, claims, &body.fields)
}

/// Reserve a server-generated intent without creating a record.
pub async fn reserve(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<EntityRequest>,
) -> Result<Response, ForgeError> {
    let scope = load_scope(&state, &schema, claims.as_ref()).await?;
    supported(&state, &scope.schema)?;
    authorize_input(&state, &scope, claims.as_ref(), &body).await?;
    let receipt = process(
        &state,
        CreateIntentRequest::Reserve {
            scope,
            fingerprint: fingerprint(&body)?,
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(receipt_json(&receipt, "pending", None)),
    )
        .into_response())
}
fn receipt_json(
    receipt: &CreateIntentReceipt,
    state: &str,
    entity_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({"id":receipt.id.as_str(),"state":state,"expires_at":receipt.expires_at,"recover_until":receipt.recover_until,"entity_id":entity_id})
}
/// Read a receipt under current scope and current record authorization.
pub async fn read(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path((schema, id)): Path<(String, String)>,
    OptionalClaims(claims): OptionalClaims,
) -> Result<Response, ForgeError> {
    let scope = load_scope(&state, &schema, claims.as_ref()).await?;
    let receipt = process(
        &state,
        CreateIntentRequest::Read {
            scope,
            id: id.parse().map_err(error)?,
        },
    )
    .await?;
    let Some(entity_id) = receipt.entity_id.as_ref() else {
        return Ok(Json(receipt_json(&receipt, "pending", None)).into_response());
    };
    match entities::get_entity(
        State(state),
        Path((schema, entity_id.to_string())),
        OptionalClaims(claims),
        Query(HashMap::new()),
    )
    .await
    {
        Ok(_) => Ok(Json(receipt_json(
            &receipt,
            "committed",
            Some(entity_id.as_str()),
        ))
        .into_response()),
        Err(ForgeError::EntityNotFound { .. }) => {
            Ok(Json(receipt_json(&receipt, "committed_unavailable", None)).into_response())
        }
        Err(error) => Err(error),
    }
}

pub(super) struct IntentCommit {
    scope: CreateIntentScope,
    id: CreateIntentId,
    fingerprint: CreateFingerprint,
    pub receipt: CreateIntentReceipt,
}
pub(super) async fn preflight(
    state: &AppState<SchemaForgeConfig>,
    schema: &SchemaDefinition,
    claims: Option<&Claims>,
    headers: &HeaderMap,
    body: &EntityRequest,
) -> Result<Option<IntentCommit>, ForgeError> {
    if !headers.contains_key(HEADER) {
        return Ok(None);
    }
    let scope = scope(state, schema.clone(), claims).await?;
    authorize_input(state, &scope, claims, body).await?;
    if headers.get_all(HEADER).iter().count() != 1 {
        return Err(error(CreateIntentError::Invalid));
    }
    let id: CreateIntentId = headers[HEADER]
        .to_str()
        .map_err(|_| error(CreateIntentError::Invalid))?
        .parse()
        .map_err(error)?;
    let fingerprint = fingerprint(body)?;
    let receipt = process(
        state,
        CreateIntentRequest::Read {
            scope: scope.clone(),
            id: id.clone(),
        },
    )
    .await?;
    if receipt.fingerprint != fingerprint {
        return Err(error(CreateIntentError::ContentConflict));
    }
    if receipt.entity_id.is_none() {
        supported(state, &scope.schema)?;
    }
    if receipt.entity_id.is_none() && !receipt.definition_matches {
        return Err(error(CreateIntentError::SchemaChanged));
    }
    Ok(Some(IntentCommit {
        scope,
        id,
        fingerprint,
        receipt,
    }))
}
pub(super) async fn commit(
    state: &AppState<SchemaForgeConfig>,
    intent: IntentCommit,
    entity: Entity,
) -> Result<CreateIntentReceipt, ForgeError> {
    process(
        state,
        CreateIntentRequest::Commit {
            scope: intent.scope,
            id: intent.id,
            fingerprint: intent.fingerprint,
            entity,
        },
    )
    .await
}
pub(super) async fn result(
    state: AppState<SchemaForgeConfig>,
    schema: String,
    claims: Option<Claims>,
    receipt: &CreateIntentReceipt,
) -> Result<Response, ForgeError> {
    let id = receipt
        .entity_id
        .as_ref()
        .ok_or_else(|| error(CreateIntentError::Unavailable))?;
    let response = entities::get_entity(
        State(state),
        Path((schema, id.to_string())),
        OptionalClaims(claims),
        Query(HashMap::new()),
    )
    .await;
    let mut response = match response {
        Ok(response) => response.into_response(),
        Err(ForgeError::EntityNotFound { .. }) => return Err(ForgeError::Conflict {
            reason: "create_result_unavailable",
            message:
                "The create committed, but its result is no longer available. It was not recreated."
                    .into(),
        }),
        Err(error) => return Err(error),
    };
    *response.status_mut() = if receipt.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    response.headers_mut().insert(
        HEADER,
        receipt
            .id
            .as_str()
            .parse()
            .map_err(|_| error(CreateIntentError::Invalid))?,
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn digest(value: serde_json::Value) -> CreateFingerprint {
        fingerprint(&EntityRequest {
            fields: value.as_object().unwrap().clone(),
        })
        .unwrap()
    }
    #[test]
    fn input_fingerprints_preserve_content_distinctions() {
        assert_eq!(
            digest(serde_json::from_str(r#"{"a":{"z":1,"b":2},"b":3}"#).unwrap()),
            digest(serde_json::from_str(r#"{"b":3,"a":{"b":2,"z":1}}"#).unwrap())
        );
        for (a, b) in [
            (serde_json::json!({}), serde_json::json!({"a":null})),
            (
                serde_json::json!({"a":[1,2]}),
                serde_json::json!({"a":[2,1]}),
            ),
            (serde_json::json!({"a":1}), serde_json::json!({"a":1.0})),
        ] {
            assert_ne!(digest(a), digest(b));
        }
    }
    #[tokio::test]
    async fn configured_side_effects_refuse_pending_protocol() {
        use acton_service::{config::Config, service_builder::ServiceBuilder};
        use schema_forge_core::types::{
            Annotation, FieldDefinition, FieldName, FieldType, HookEvent, SchemaId, TextConstraints,
        };
        let mut schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Note").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("title").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();
        let service = ServiceBuilder::new()
            .with_config(Config::<SchemaForgeConfig>::default())
            .build();
        assert!(supported(service.state(), &schema).is_ok());
        schema.annotations.push(Annotation::Hook {
            event: HookEvent::AfterChange,
            intent: "notify".into(),
        });
        assert!(matches!(
            supported(service.state(), &schema),
            Err(ForgeError::Conflict {
                reason: "create_intent_unsupported",
                ..
            })
        ));
        schema.annotations.clear();
        let mut config = Config::<SchemaForgeConfig>::default();
        config.custom.schema_forge.webhooks.enabled = true;
        let service = ServiceBuilder::new().with_config(config).build();
        assert!(matches!(
            supported(service.state(), &schema),
            Err(ForgeError::Conflict {
                reason: "create_intent_unsupported",
                ..
            })
        ));
    }
}
