//! Authenticated change streams with live identity checks and canonical read projection.
use super::{
    auth::account_username,
    entities,
    query_params::{parse_filter_key, parse_filter_params, FilterOp},
};
use crate::{
    access::{check_schema_access, AccessAction, OptionalClaims},
    config::SchemaForgeConfig,
    error::ForgeError,
    events::{self, CommittedSnapshot, EventsRuntime, Subscribe, Subscription},
    messages::{GetSchema, GetTenantConfig, ReplyChannel},
    middleware::tenant_scope,
    ForgeActor,
};
use acton_service::{
    middleware::Claims,
    prelude::ActorHandleInterface,
    sse::{Event, KeepAlive, Sse},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use futures::stream;
use schema_forge_backend::{user_store::TenantRole, Entity, TenantRef};
use schema_forge_core::{
    query::Filter,
    types::{DynamicValue, SchemaDefinition},
};
use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;

fn invalid_filter() -> ForgeError {
    ForgeError::InvalidQuery {
        message: "Events support equality filters on known readable fields only.".into(),
    }
}
async fn filters(
    state: &AppState<SchemaForgeConfig>,
    schema: &SchemaDefinition,
    claims: &Claims,
    params: &HashMap<String, String>,
) -> Result<Vec<(String, DynamicValue)>, ForgeError> {
    let store = entities::fetch_export_policy_store(state).await?;
    let probe = Entity::new(schema.name.clone(), Default::default());
    let mut result = Vec::new();
    for (key, value) in params {
        let (name, op) = parse_filter_key(key).ok_or_else(invalid_filter)?;
        let field = schema.field(name).ok_or_else(invalid_filter)?;
        if op != FilterOp::Eq || field.is_hidden() || name.contains('.') {
            return Err(invalid_filter());
        }
        if field.field_access().is_some()
            && !crate::authz::authorize_field(
                &store,
                Some(claims),
                schema,
                &probe,
                name,
                crate::authz::FieldDirection::Read,
            )
            .is_ok_and(|d| d.is_allow())
        {
            return Err(ForgeError::Forbidden {
                message: "Not authorized to filter this field.".into(),
            });
        }
        let one = HashMap::from([(key.clone(), value.clone())]);
        match parse_filter_params(&one, schema).map_err(|_| invalid_filter())? {
            Some(Filter::Eq { value, .. }) => result.push((name.to_owned(), value)),
            _ => return Err(invalid_filter()),
        }
    }
    Ok(result)
}
fn visible_value_matches(value: Option<&DynamicValue>, expected: &DynamicValue) -> bool {
    match (value, expected) {
        // The list parser deliberately leaves relation IDs as text; GET exposes
        // each stored Ref as the same bare ID, so compare that visible identity.
        (Some(DynamicValue::Ref(id)), DynamicValue::Text(expected)) => id.as_str() == expected,
        (Some(value), expected) => value == expected,
        _ => false,
    }
}
#[derive(serde::Serialize)]
struct SubscriberEvent<'a> {
    event_id: &'a str,
    event_type: &'a str,
    schema: &'a str,
    entity_id: &'a str,
    timestamp: &'a str,
    actor: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entity: Option<entities::EntityResponse>,
}
struct StreamState {
    state: AppState<SchemaForgeConfig>,
    runtime: Arc<EventsRuntime>,
    schema: SchemaDefinition,
    claims: Claims,
    headers: HeaderMap,
    filters: Vec<(String, DynamicValue)>,
    subscription: Subscription,
    local_account: bool,
    effective_chain: Vec<TenantRef>,
    retry: bool,
    done: bool,
    ticker: tokio::time::Interval,
}
/// Roles the request claims must still carry: the account's current global
/// roles plus, for a tenant-scoped caller, the active membership's current
/// scoped role, exactly as `tenant_scope` projects them.
fn expected_roles<'a>(
    global: &'a [String],
    tenant_roles: &'a [TenantRole],
    active: Option<&'a TenantRef>,
) -> BTreeSet<&'a str> {
    let mut roles: BTreeSet<&str> = global.iter().map(String::as_str).collect();
    if let Some(active) = active {
        roles.extend(tenant_scope::active_membership_roles(tenant_roles, active));
    }
    roles
}
impl StreamState {
    /// The active tenant the middleware scoped this request to, if any.
    /// Platform administrators bypass tenant scope, so their `tenant_chain`
    /// is the login-time membership set and names no active tenant.
    fn active_tenant(&self) -> Option<&TenantRef> {
        self.effective_chain
            .last()
            .filter(|_| tenant_scope::is_tenant_scoped(&self.claims))
    }
    async fn identity_valid(&self) -> bool {
        if self.claims.exp <= chrono::Utc::now().timestamp() {
            return false;
        }
        let username = account_username(&self.claims);
        let user = match self.runtime.auth_store.get_user(username).await {
            Ok(Some(user)) if user.active => user,
            Ok(None) if !self.local_account && self.effective_chain.is_empty() => return true,
            _ => return false,
        };
        let active = self.active_tenant();
        let tenant_roles = match active {
            Some(_) => match self.runtime.auth_store.list_tenant_roles(username).await {
                Ok(roles) => roles,
                Err(_) => return false,
            },
            None => Vec::new(),
        };
        let current: BTreeSet<&str> = self.claims.roles.iter().map(String::as_str).collect();
        if expected_roles(&user.roles, &tenant_roles, active) != current {
            return false;
        }
        let Some(leaf) = active else {
            return true;
        };
        let Ok(memberships) = self
            .runtime
            .auth_store
            .list_tenant_memberships(username)
            .await
        else {
            return false;
        };
        if !memberships.contains(leaf) {
            return false;
        }
        let Some(forge) = self.state.actor::<ForgeActor>() else {
            return false;
        };
        let (tx, rx) = oneshot::channel();
        forge
            .send(GetTenantConfig {
                reply: ReplyChannel::new(tx),
            })
            .await;
        let Ok(Ok(config)) = tokio::time::timeout(Duration::from_secs(5), rx).await else {
            return false;
        };
        let Some(config) = config else {
            return false;
        };
        match tenant_scope::walk_to_root(leaf, &config, self.runtime.entity_store.as_ref()).await {
            Ok(chain) => chain == self.effective_chain,
            Err(tenant_scope::WalkError::EntityMissing { .. }) => {
                self.effective_chain == vec![leaf.clone()]
            }
            Err(_) => false,
        }
    }
    fn close(&mut self, reason: &'static str) -> Event {
        self.done = true;
        Event::default()
            .event("closed")
            .data(serde_json::json!({"reason": reason}).to_string())
    }
    async fn next(&mut self) -> Option<Event> {
        if self.done {
            return None;
        }
        if self.retry {
            self.retry = false;
            return Some(
                Event::default().retry(Duration::from_secs(self.runtime.config.retry_secs)),
            );
        }
        loop {
            let message = tokio::select! {
                _ = self.ticker.tick() => { if !self.identity_valid().await { return Some(self.close("authorization_changed")); } continue; }
                message = self.subscription.receiver.recv() => match message {
                    Ok(message) => message,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return Some(self.close("slow_consumer")),
                    Err(_) => return None,
                }
            };
            if !self.identity_valid().await {
                return Some(self.close("authorization_changed"));
            }
            let Ok(snapshot) = serde_json::from_str::<CommittedSnapshot>(&message.data) else {
                return Some(self.close("stream_error"));
            };
            if snapshot.schema != self.schema.name.as_str() {
                continue;
            }
            let Some(forge) = self.state.actor::<ForgeActor>() else {
                return Some(self.close("stream_error"));
            };
            let (tx, rx) = oneshot::channel();
            forge
                .send(GetSchema {
                    name: snapshot.schema.clone(),
                    reply: ReplyChannel::new(tx),
                })
                .await;
            let Ok(Ok(Some(schema))) = tokio::time::timeout(Duration::from_secs(5), rx).await
            else {
                return Some(self.close("schema_changed"));
            };
            self.schema = schema;
            let Ok(entity) = snapshot.entity() else {
                return Some(self.close("stream_error"));
            };
            // Record authorization always sees the full committed/pre-delete snapshot.
            let projection = entities::project_read_snapshot(
                &self.state,
                &self.schema,
                entity,
                Some(&self.claims),
                &self.headers,
            )
            .await;
            let (response, visible, _) = match projection {
                Ok(value) => value,
                Err(ForgeError::Forbidden { .. }) => continue,
                Err(_) => return Some(self.close("stream_error")),
            };
            if !self.filters.iter().all(|(name, value)| {
                response.fields.contains_key(name)
                    && visible_value_matches(visible.fields.get(name), value)
            }) {
                continue;
            }
            let data = SubscriberEvent {
                event_id: &snapshot.event_id,
                event_type: &snapshot.event_type,
                schema: &snapshot.schema,
                entity_id: snapshot.entity_id.as_str(),
                timestamp: &snapshot.timestamp,
                actor: snapshot.actor.as_deref(),
                entity: (snapshot.event_type != "entity.deleted").then_some(response),
            };
            return Some(
                match Event::default()
                    .id(&snapshot.event_id)
                    .event(&snapshot.event_type)
                    .json_data(&data)
                {
                    Ok(event) => event,
                    Err(_) => self.close("stream_error"),
                },
            );
        }
    }
}
/// GET /schemas/{schema}/events. No replay and no credentials in query parameters.
pub async fn subscribe(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(name): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ForgeError> {
    let forge = state
        .actor::<ForgeActor>()
        .ok_or_else(|| ForgeError::Internal {
            message: "ForgeActor not registered".into(),
        })?;
    let runtime = events::runtime(&forge)
        .await?
        .ok_or_else(|| ForgeError::EntityNotFound {
            schema: "events".into(),
            entity_id: "disabled".into(),
        })?;
    let claims = claims.ok_or_else(|| ForgeError::Unauthorized {
        message: "Authentication required.".into(),
    })?;
    let (tx, rx) = oneshot::channel();
    forge
        .send(GetSchema {
            name: name.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let schema = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| ForgeError::Internal {
            message: "Forge actor timeout".into(),
        })?
        .map_err(|_| ForgeError::Internal {
            message: "Forge actor unavailable".into(),
        })?
        .ok_or(ForgeError::SchemaNotFound { name })?;
    let store = entities::fetch_export_policy_store(&state).await?;
    check_schema_access(&store, &schema, Some(&claims), AccessAction::Read)?;
    let filters = filters(&state, &schema, &claims, &params).await?;
    let local_account = runtime
        .auth_store
        .get_user(account_username(&claims))
        .await
        .map_err(ForgeError::from)?
        .is_some();
    let effective_chain = claims.custom_claim_as("tenant_chain").unwrap_or_default();
    let (tx, rx) = oneshot::channel();
    forge
        .send(Subscribe {
            subject: claims.sub.clone(),
            reply: ReplyChannel::new(tx),
        })
        .await;
    let subscription = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| ForgeError::Internal {
            message: "Forge actor timeout".into(),
        })?
        .map_err(|_| ForgeError::Internal {
            message: "Forge actor unavailable".into(),
        })??;
    let keep_alive = Duration::from_secs(runtime.config.keep_alive_secs);
    let context = StreamState {
        state,
        runtime,
        schema,
        claims,
        headers,
        filters,
        subscription,
        local_account,
        effective_chain,
        retry: true,
        done: false,
        ticker: tokio::time::interval(keep_alive.min(Duration::from_secs(5))),
    };
    if !context.identity_valid().await {
        return Err(ForgeError::Forbidden {
            message: "Stream authorization is no longer valid.".into(),
        });
    }
    let stream = stream::unfold(context, |mut context| async move {
        context
            .next()
            .await
            .map(|event| (Ok::<_, Infallible>(event), context))
    });
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(keep_alive).text("keep-alive"))
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().expect("static header"));
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().expect("static header"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::PLATFORM_ADMIN_ROLE;

    fn tenant(id: &str) -> TenantRef {
        TenantRef {
            schema: "Organization".into(),
            entity_id: id.into(),
        }
    }
    fn grant(id: &str, role: &str) -> TenantRole {
        TenantRole {
            tenant: tenant(id),
            role: role.into(),
        }
    }
    fn claims(sub: &str, roles: &[&str]) -> Claims {
        serde_json::from_value(serde_json::json!({
            "sub": sub, "roles": roles, "perms": [], "exp": 9_999_999_999_u64
        }))
        .unwrap()
    }

    #[test]
    fn expected_roles_add_only_the_active_membership_role() {
        let global = vec!["member".to_owned()];
        let grants = [
            grant("alpha", "owner"),
            grant("beta", "auditor"),
            grant("alpha", PLATFORM_ADMIN_ROLE),
        ];
        let alpha = tenant("alpha");
        assert_eq!(
            expected_roles(&global, &grants, Some(&alpha)),
            BTreeSet::from(["member", "owner"])
        );
        assert_eq!(
            expected_roles(&global, &grants, None),
            BTreeSet::from(["member"])
        );
        let lobby = tenant("lobby");
        assert_eq!(
            expected_roles(&global, &grants, Some(&lobby)),
            BTreeSet::from(["member"])
        );
    }

    #[test]
    fn platform_admins_are_never_tenant_scoped() {
        assert!(!tenant_scope::is_tenant_scoped(&claims(
            "user:admin",
            &[PLATFORM_ADMIN_ROLE]
        )));
        assert!(tenant_scope::is_tenant_scoped(&claims(
            "user:solo",
            &["member"]
        )));
    }
}
