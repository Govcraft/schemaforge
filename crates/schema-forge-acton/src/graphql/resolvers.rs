use std::collections::BTreeMap;

use async_graphql::dynamic::{FieldValue, ResolverContext};
use async_graphql::{ErrorExtensions, Value as GqlValue};
use schema_forge_backend::entity::Entity;
use schema_forge_core::query::{validate_filter, FieldPath, SortOrder};
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition, SchemaName};

use super::context::ForgeGraphqlContext;
use super::input_types::{
    filter_input_to_filter, gql_input_to_entity_fields, gql_input_to_partial_fields,
};
use crate::access::{
    check_schema_access, filter_entity_fields, inject_tenant_on_create, inject_tenant_scope,
    AccessAction, FieldFilterDirection,
};
use crate::error::ForgeError;

/// Entity data stored in resolver parent values.
pub struct EntityFields {
    pub id: EntityId,
    pub schema: SchemaName,
    pub fields: BTreeMap<String, DynamicValue>,
}

/// Convert ForgeError to async_graphql::Error with extension codes.
pub fn forge_error_to_gql(err: ForgeError) -> async_graphql::Error {
    let code = match &err {
        ForgeError::SchemaNotFound { .. } | ForgeError::EntityNotFound { .. } => "NOT_FOUND",
        ForgeError::Forbidden { .. } => "FORBIDDEN",
        ForgeError::Unauthorized { .. } => "UNAUTHORIZED",
        ForgeError::ValidationFailed { .. } => "VALIDATION_ERROR",
        ForgeError::InvalidQuery { .. }
        | ForgeError::InvalidSchemaName { .. }
        | ForgeError::InvalidEntityId { .. } => "BAD_REQUEST",
        _ => "INTERNAL_ERROR",
    };
    async_graphql::Error::new(err.to_string()).extend_with(|_, e| e.set("code", code))
}

/// Resolve a single entity by ID.
pub async fn resolve_get_entity<'a>(
    ctx: &ResolverContext<'a>,
    schema_name: &str,
    schema_def: &SchemaDefinition,
    type_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(schema_def, auth, AccessAction::Read).map_err(forge_error_to_gql)?;

    let id_arg = ctx.args.try_get("id")?.string()?.to_string();

    let schema = SchemaName::new(schema_name).map_err(|_| {
        forge_error_to_gql(ForgeError::InvalidSchemaName {
            name: schema_name.to_string(),
        })
    })?;

    let entity_id = EntityId::parse(&id_arg)
        .map_err(|_| forge_error_to_gql(ForgeError::InvalidEntityId { id: id_arg.clone() }))?;

    let mut entity = match gql_ctx.state.backend.get(&schema, &entity_id).await {
        Ok(e) => e,
        Err(e) => return Err(forge_error_to_gql(ForgeError::from(e))),
    };

    // Record-level visibility check
    if let (Some(ref policy), Some(auth_ctx)) = (&gql_ctx.state.record_access_policy, auth) {
        let visible = policy
            .filter_visible(schema_def, auth_ctx, vec![entity.clone()])
            .await;
        if visible.is_empty() {
            return Err(forge_error_to_gql(ForgeError::Forbidden {
                message: format!("not authorized to view entity '{id_arg}'"),
            }));
        }
    }

    filter_entity_fields(&mut entity, schema_def, auth, FieldFilterDirection::Read);

    Ok(Some(entity_to_field_value(entity, type_name)))
}

/// Resolve a list of entities with filter/sort/pagination.
pub async fn resolve_list_entities<'a>(
    ctx: &ResolverContext<'a>,
    _schema_name: &str,
    schema_def: &SchemaDefinition,
    type_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(schema_def, auth, AccessAction::Read).map_err(forge_error_to_gql)?;

    let mut query = schema_forge_core::query::Query::new(schema_def.id.clone());

    // Parse limit
    if let Some(limit_val) = ctx.args.get("limit") {
        let limit = limit_val.i64()? as usize;
        query = query.with_limit(limit);
    }

    // Parse offset
    if let Some(offset_val) = ctx.args.get("offset") {
        let offset = offset_val.i64()? as usize;
        query = query.with_offset(offset);
    }

    // Parse filter
    if let Some(filter_accessor) = ctx.args.get("filter") {
        let filter_obj = filter_accessor.object()?;
        let filter =
            filter_input_to_filter(filter_obj.as_index_map(), schema_def).map_err(|errors| {
                forge_error_to_gql(ForgeError::InvalidQuery {
                    message: errors.join("; "),
                })
            })?;
        validate_filter(&filter, schema_def).map_err(|errors| {
            forge_error_to_gql(ForgeError::InvalidQuery {
                message: errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            })
        })?;
        query = query.with_filter(filter);
    }

    // Parse sort
    if let Some(sort_accessor) = ctx.args.get("sort") {
        let sort_list = sort_accessor.list()?;
        for i in 0..sort_list.len() {
            if let Ok(item) = sort_list.try_get(i) {
                if let Ok(obj) = item.object() {
                    let field_str = obj.try_get("field")?.enum_name()?.to_string();
                    let order = match obj.get("order") {
                        Some(o) => match o.enum_name() {
                            Ok("DESC") => SortOrder::Descending,
                            _ => SortOrder::Ascending,
                        },
                        None => SortOrder::Ascending,
                    };
                    if let Ok(path) = FieldPath::parse(&field_str) {
                        query = query.with_sort(path, order);
                    }
                }
            }
        }
    }

    // Inject tenant scope
    inject_tenant_scope(&mut query, auth, &gql_ctx.state.tenant_config);

    let result = gql_ctx
        .state
        .backend
        .query(&query)
        .await
        .map_err(|e| forge_error_to_gql(ForgeError::from(e)))?;

    // Record-level access filtering
    let visible_entities =
        if let (Some(ref policy), Some(auth_ctx)) = (&gql_ctx.state.record_access_policy, auth) {
            policy
                .filter_visible(schema_def, auth_ctx, result.entities)
                .await
        } else {
            result.entities
        };

    let count = visible_entities.len();
    let total_count = result.total_count;

    let items: Vec<EntityFields> = visible_entities
        .into_iter()
        .map(|mut entity| {
            filter_entity_fields(&mut entity, schema_def, auth, FieldFilterDirection::Read);
            EntityFields {
                id: entity.id.clone(),
                schema: entity.schema.clone(),
                fields: entity.fields,
            }
        })
        .collect();

    Ok(Some(FieldValue::owned_any(ConnectionData {
        items,
        type_name: type_name.to_string(),
        count,
        total_count,
    })))
}

/// Data for a connection response.
pub struct ConnectionData {
    pub items: Vec<EntityFields>,
    pub type_name: String,
    pub count: usize,
    pub total_count: Option<usize>,
}

/// Resolve create entity mutation.
pub async fn resolve_create_entity<'a>(
    ctx: &ResolverContext<'a>,
    schema_name: &str,
    schema_def: &SchemaDefinition,
    type_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(schema_def, auth, AccessAction::Write).map_err(forge_error_to_gql)?;

    let input_accessor = ctx.args.try_get("input")?;
    let input_obj = input_accessor.object()?;

    let mut fields = gql_input_to_entity_fields(input_obj.as_index_map(), schema_def)
        .map_err(|errors| forge_error_to_gql(ForgeError::ValidationFailed { details: errors }))?;

    // Inject tenant
    inject_tenant_on_create(&mut fields, auth, &gql_ctx.state.tenant_config);

    let schema = SchemaName::new(schema_name).map_err(|_| {
        forge_error_to_gql(ForgeError::InvalidSchemaName {
            name: schema_name.to_string(),
        })
    })?;

    let mut entity = Entity::new(schema, fields);
    filter_entity_fields(&mut entity, schema_def, auth, FieldFilterDirection::Write);

    let mut created = gql_ctx
        .state
        .backend
        .create(&entity)
        .await
        .map_err(|e| forge_error_to_gql(ForgeError::from(e)))?;

    filter_entity_fields(&mut created, schema_def, auth, FieldFilterDirection::Read);

    Ok(Some(entity_to_field_value(created, type_name)))
}

/// Resolve update entity mutation.
pub async fn resolve_update_entity<'a>(
    ctx: &ResolverContext<'a>,
    schema_name: &str,
    schema_def: &SchemaDefinition,
    type_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(schema_def, auth, AccessAction::Write).map_err(forge_error_to_gql)?;

    let id_arg = ctx.args.try_get("id")?.string()?.to_string();

    let schema = SchemaName::new(schema_name).map_err(|_| {
        forge_error_to_gql(ForgeError::InvalidSchemaName {
            name: schema_name.to_string(),
        })
    })?;

    let entity_id = EntityId::parse(&id_arg)
        .map_err(|_| forge_error_to_gql(ForgeError::InvalidEntityId { id: id_arg.clone() }))?;

    // Record-level ownership check
    if let (Some(ref policy), Some(auth_ctx)) = (&gql_ctx.state.record_access_policy, auth) {
        let existing = gql_ctx
            .state
            .backend
            .get(&schema, &entity_id)
            .await
            .map_err(|e| forge_error_to_gql(ForgeError::from(e)))?;
        if !policy.can_modify(schema_def, auth_ctx, &existing).await {
            return Err(forge_error_to_gql(ForgeError::Forbidden {
                message: format!("not authorized to modify entity '{id_arg}'"),
            }));
        }
    }

    let input_accessor = ctx.args.try_get("input")?;
    let input_obj = input_accessor.object()?;

    let fields = gql_input_to_partial_fields(input_obj.as_index_map(), schema_def)
        .map_err(|errors| forge_error_to_gql(ForgeError::ValidationFailed { details: errors }))?;

    let mut entity = Entity::with_id(entity_id, schema, fields);
    filter_entity_fields(&mut entity, schema_def, auth, FieldFilterDirection::Write);

    let mut updated = gql_ctx
        .state
        .backend
        .update(&entity)
        .await
        .map_err(|e| forge_error_to_gql(ForgeError::from(e)))?;

    filter_entity_fields(&mut updated, schema_def, auth, FieldFilterDirection::Read);

    Ok(Some(entity_to_field_value(updated, type_name)))
}

/// Resolve delete entity mutation.
pub async fn resolve_delete_entity(
    ctx: &ResolverContext<'_>,
    schema_name: &str,
    schema_def: &SchemaDefinition,
) -> async_graphql::Result<GqlValue> {
    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(schema_def, auth, AccessAction::Delete).map_err(forge_error_to_gql)?;

    let id_arg = ctx.args.try_get("id")?.string()?.to_string();

    let schema = SchemaName::new(schema_name).map_err(|_| {
        forge_error_to_gql(ForgeError::InvalidSchemaName {
            name: schema_name.to_string(),
        })
    })?;

    let entity_id = EntityId::parse(&id_arg)
        .map_err(|_| forge_error_to_gql(ForgeError::InvalidEntityId { id: id_arg.clone() }))?;

    // Record-level ownership check
    if let (Some(ref policy), Some(auth_ctx)) = (&gql_ctx.state.record_access_policy, auth) {
        let entity = gql_ctx
            .state
            .backend
            .get(&schema, &entity_id)
            .await
            .map_err(|e| forge_error_to_gql(ForgeError::from(e)))?;
        if !policy.can_delete(schema_def, auth_ctx, &entity).await {
            return Err(forge_error_to_gql(ForgeError::Forbidden {
                message: format!("not authorized to delete entity '{id_arg}'"),
            }));
        }
    }

    gql_ctx
        .state
        .backend
        .delete(&schema, &entity_id)
        .await
        .map_err(|e| forge_error_to_gql(ForgeError::from(e)))?;

    Ok(GqlValue::Boolean(true))
}

/// Resolve a relation field (Cardinality::One).
pub async fn resolve_relation_one<'a>(
    ctx: &ResolverContext<'a>,
    parent: &EntityFields,
    field_name: &str,
    target_schema_name: &str,
    target_schema_def: &SchemaDefinition,
    target_type_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let ref_id = match parent.fields.get(field_name) {
        Some(DynamicValue::Ref(id)) => id.clone(),
        Some(DynamicValue::Null) | None => return Ok(None),
        _ => return Ok(None),
    };

    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(target_schema_def, auth, AccessAction::Read).map_err(forge_error_to_gql)?;

    let target_schema = SchemaName::new(target_schema_name).map_err(|_| {
        forge_error_to_gql(ForgeError::InvalidSchemaName {
            name: target_schema_name.to_string(),
        })
    })?;

    let mut entity = match gql_ctx.state.backend.get(&target_schema, &ref_id).await {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    filter_entity_fields(
        &mut entity,
        target_schema_def,
        auth,
        FieldFilterDirection::Read,
    );

    Ok(Some(entity_to_field_value(entity, target_type_name)))
}

/// Resolve a relation field (Cardinality::Many).
pub async fn resolve_relation_many<'a>(
    ctx: &ResolverContext<'a>,
    parent: &EntityFields,
    field_name: &str,
    target_schema_name: &str,
    target_schema_def: &SchemaDefinition,
    target_type_name: &str,
) -> async_graphql::Result<Option<FieldValue<'a>>> {
    let ref_ids = match parent.fields.get(field_name) {
        Some(DynamicValue::RefArray(ids)) => ids.clone(),
        Some(DynamicValue::Null) | None => {
            return Ok(Some(FieldValue::list(Vec::<FieldValue>::new())))
        }
        _ => return Ok(Some(FieldValue::list(Vec::<FieldValue>::new()))),
    };

    let gql_ctx = ctx.data::<ForgeGraphqlContext>()?;
    let auth = gql_ctx.auth.as_ref();

    check_schema_access(target_schema_def, auth, AccessAction::Read).map_err(forge_error_to_gql)?;

    let target_schema = SchemaName::new(target_schema_name).map_err(|_| {
        forge_error_to_gql(ForgeError::InvalidSchemaName {
            name: target_schema_name.to_string(),
        })
    })?;

    let mut results = Vec::new();
    for ref_id in ref_ids {
        if let Ok(mut entity) = gql_ctx.state.backend.get(&target_schema, &ref_id).await {
            filter_entity_fields(
                &mut entity,
                target_schema_def,
                auth,
                FieldFilterDirection::Read,
            );
            results.push(entity_to_field_value(entity, target_type_name));
        }
    }

    Ok(Some(FieldValue::list(results)))
}

/// Convert an Entity to a FieldValue wrapping EntityFields.
fn entity_to_field_value(entity: Entity, type_name: &str) -> FieldValue<'static> {
    FieldValue::owned_any(EntityFields {
        id: entity.id.clone(),
        schema: entity.schema.clone(),
        fields: entity.fields,
    })
    .with_type(type_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extension_code(err: &async_graphql::Error) -> Option<String> {
        err.extensions
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(|v| {
                if let GqlValue::String(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
    }

    #[test]
    fn forge_error_to_gql_not_found() {
        let err = ForgeError::SchemaNotFound { name: "X".into() };
        let gql_err = forge_error_to_gql(err);
        assert!(gql_err.message.contains("not found"));
        assert_eq!(extension_code(&gql_err).as_deref(), Some("NOT_FOUND"));
    }

    #[test]
    fn forge_error_to_gql_forbidden() {
        let err = ForgeError::Forbidden {
            message: "denied".into(),
        };
        let gql_err = forge_error_to_gql(err);
        assert_eq!(extension_code(&gql_err).as_deref(), Some("FORBIDDEN"));
    }

    #[test]
    fn forge_error_to_gql_validation() {
        let err = ForgeError::ValidationFailed {
            details: vec!["bad field".into()],
        };
        let gql_err = forge_error_to_gql(err);
        assert_eq!(
            extension_code(&gql_err).as_deref(),
            Some("VALIDATION_ERROR")
        );
    }

    #[test]
    fn forge_error_to_gql_bad_request() {
        let err = ForgeError::InvalidQuery {
            message: "bad filter".into(),
        };
        let gql_err = forge_error_to_gql(err);
        assert_eq!(extension_code(&gql_err).as_deref(), Some("BAD_REQUEST"));
    }

    #[test]
    fn forge_error_to_gql_internal() {
        let err = ForgeError::Internal {
            message: "oops".into(),
        };
        let gql_err = forge_error_to_gql(err);
        assert_eq!(extension_code(&gql_err).as_deref(), Some("INTERNAL_ERROR"));
    }
}
