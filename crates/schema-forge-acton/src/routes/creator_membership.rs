//! Prepare the server-owned membership before the supervised atomic create.
use std::collections::BTreeMap;

use acton_service::{middleware::Claims, prelude::ActorHandleInterface, state::AppState};
use schema_forge_backend::Entity;
use schema_forge_core::{
    query::{FieldPath, Filter, Query},
    types::{DynamicValue, SchemaDefinition, SchemaName},
};
use tokio::sync::oneshot;

use crate::{
    actor::ForgeActor,
    config::SchemaForgeConfig,
    error::ForgeError,
    messages::{GetSchema, QueryEntities, ReplyChannel},
};

pub(super) async fn prepare(
    state: &AppState<SchemaForgeConfig>,
    schema: &SchemaDefinition,
    root: &Entity,
    claims: Option<&Claims>,
) -> Result<Option<Entity>, ForgeError> {
    let Some(role) = state
        .config()
        .custom
        .schema_forge
        .tenancy
        .creator_role
        .as_deref()
        .filter(|_| crate::access::is_tenant_root(schema))
    else {
        return Ok(None);
    };
    let claims = claims.ok_or_else(|| ForgeError::Unauthorized {
        message: "tenant root creator membership requires an authenticated User".into(),
    })?;
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: "User".into(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let user_schema =
        super::entities::ask_forge(rx)
            .await?
            .ok_or_else(|| ForgeError::Internal {
                message: "User schema is required for creator membership".into(),
            })?;
    let username = crate::authz::adapters::user_id_from_sub(&claims.sub);
    let query = Query::new(user_schema.id)
        .with_filter(Filter::eq(
            FieldPath::single("email"),
            DynamicValue::Text(username.into()),
        ))
        .with_limit(2);
    let (tx, rx) = oneshot::channel();
    forge
        .send(QueryEntities {
            query,
            reply: ReplyChannel::new(tx),
        })
        .await;
    let users = super::entities::ask_forge(rx)
        .await?
        .map_err(ForgeError::from)?;
    let [user] = users.entities.as_slice() else {
        return Err(ForgeError::Unauthorized {
            message: "tenant root creator must resolve to exactly one durable User".into(),
        });
    };
    let membership_schema =
        SchemaName::new("TenantMembership").map_err(|e| ForgeError::Internal {
            message: e.to_string(),
        })?;
    Ok(Some(Entity::new(
        membership_schema,
        BTreeMap::from([
            ("user".into(), DynamicValue::Ref(user.id.clone())),
            (
                "tenant_type".into(),
                DynamicValue::Text(root.schema.to_string()),
            ),
            ("tenant_id".into(), DynamicValue::Text(root.id.to_string())),
            ("role".into(), DynamicValue::Text(role.into())),
        ]),
    )))
}
