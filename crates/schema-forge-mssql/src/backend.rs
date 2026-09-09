use std::cmp::Ordering;
use std::collections::BTreeMap;

use acton_service::config::DatabaseConfig;
use acton_service::mssql::{create_pool, MssqlPool};
use schema_forge_backend::{BackendError, Entity, EntityStore, QueryResult, SchemaBackend};
use schema_forge_core::migration::{MigrationStep, ValueTransform};
use schema_forge_core::query::{
    AggregateOp, AggregateQuery, AggregateResult, FieldPath, Filter, Query, SortOrder,
};
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition, SchemaName};

const METADATA: &str = "_schema_metadata";

/// SQL Server backend using lossless JSON documents in ordinary SQL tables.
#[derive(Clone)]
pub struct MssqlBackend {
    pool: MssqlPool,
}

impl MssqlBackend {
    /// Connect using acton-service's SQL Server configuration. This honors
    /// both connection-string credentials and `mssql_auth = "integrated"`.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, BackendError> {
        let pool = create_pool(config)
            .await
            .map_err(|error| BackendError::ConnectionError {
                message: error.to_string(),
            })?;
        let backend = Self { pool };
        backend.ensure_metadata().await?;
        Ok(backend)
    }

    /// Reuse an existing acton-service SQL Server pool.
    pub async fn from_pool(pool: MssqlPool) -> Result<Self, BackendError> {
        let backend = Self { pool };
        backend.ensure_metadata().await?;
        Ok(backend)
    }

    /// Return the underlying pool.
    pub fn pool(&self) -> &MssqlPool {
        &self.pool
    }

    async fn ensure_metadata(&self) -> Result<(), BackendError> {
        let sql = format!(
            "IF OBJECT_ID(N'[dbo].[{METADATA}]', N'U') IS NULL \
             CREATE TABLE [dbo].[{METADATA}] (\
             [name] NVARCHAR(255) NOT NULL PRIMARY KEY, \
             [definition] NVARCHAR(MAX) NOT NULL CHECK (ISJSON([definition]) = 1));"
        );
        let mut connection = connection(&self.pool).await?;
        connection.execute(sql, &[]).await.map_err(query_error)?;
        Ok(())
    }

    async fn schema_for_query(&self, query: &Query) -> Result<SchemaDefinition, BackendError> {
        self.list_schema_metadata()
            .await?
            .into_iter()
            .find(|schema| schema.id == query.schema)
            .ok_or_else(|| BackendError::SchemaNotFound {
                schema: query.schema.to_string(),
            })
    }
}

impl SchemaBackend for MssqlBackend {
    async fn apply_migration(
        &self,
        schema_name: &SchemaName,
        steps: &[MigrationStep],
    ) -> Result<(), BackendError> {
        if steps.iter().any(|step| {
            matches!(
                step,
                MigrationStep::ChangeType {
                    transform: ValueTransform::NullRemovedEnumVariants { .. },
                    ..
                }
            )
        }) {
            if let Some(schema) = self.load_schema_metadata(schema_name).await? {
                for step in steps {
                    if let MigrationStep::ChangeType {
                        name,
                        transform: ValueTransform::NullRemovedEnumVariants { .. },
                        ..
                    } = step
                    {
                        if schema
                            .field(name.as_str())
                            .is_some_and(|field| field.is_required())
                        {
                            return Err(BackendError::MigrationFailed { step: step.to_string(), reason: "cannot null removed variants of a required enum; migrate affected values or make the field optional first".into() });
                        }
                    }
                }
            }
        }
        let mut connection = connection(&self.pool).await?;
        for step in steps {
            if let MigrationStep::ChangeType {
                name,
                transform: ValueTransform::NullRemovedEnumVariants { variants },
                ..
            } = step
            {
                // JSON payloads store tagged DynamicValue objects. Bind both the
                // path and variant so enum strings cannot become SQL syntax.
                let path = format!("$.\"{}\"", name.as_str());
                let value_path = format!("{path}.value");
                let sql = format!("UPDATE [dbo].{} SET [data] = JSON_MODIFY([data], @P1, JSON_QUERY(N'{{\"type\":\"Null\"}}')) WHERE JSON_VALUE([data], @P2) COLLATE Latin1_General_100_BIN2 = @P3;", quote(schema_name.as_str()));
                for variant in variants {
                    connection
                        .execute(
                            &sql,
                            &[&path.as_str(), &value_path.as_str(), &variant.as_str()],
                        )
                        .await
                        .map_err(query_error)?;
                }
                continue;
            }
            let sql = match step {
                MigrationStep::CreateSchema { name, .. } => Some(format!(
                    "IF OBJECT_ID(N'[dbo].{}', N'U') IS NULL CREATE TABLE [dbo].{} \
                     ([id] NVARCHAR(255) NOT NULL PRIMARY KEY, \
                     [data] NVARCHAR(MAX) NOT NULL CHECK (ISJSON([data]) = 1));",
                    quote(name.as_str()),
                    quote(name.as_str())
                )),
                MigrationStep::DropSchema { name } => Some(format!(
                    "IF OBJECT_ID(N'[dbo].{}', N'U') IS NOT NULL DROP TABLE [dbo].{};",
                    quote(name.as_str()),
                    quote(name.as_str())
                )),
                _ => None,
            };
            if let Some(sql) = sql {
                connection.execute(sql, &[]).await.map_err(|error| {
                    BackendError::MigrationFailed {
                        step: step.to_string(),
                        reason: error.to_string(),
                    }
                })?;
            }
        }
        let _ = schema_name;
        Ok(())
    }

    async fn store_schema_metadata(
        &self,
        definition: &SchemaDefinition,
    ) -> Result<(), BackendError> {
        let json = serde_json::to_string(definition).map_err(json_error)?;
        let sql = format!(
            "MERGE [dbo].[{METADATA}] AS target \
             USING (SELECT @P1 AS [name], @P2 AS [definition]) source \
             ON target.[name] = source.[name] \
             WHEN MATCHED THEN UPDATE SET [definition] = source.[definition] \
             WHEN NOT MATCHED THEN INSERT ([name], [definition]) \
             VALUES (source.[name], source.[definition]);"
        );
        let mut connection = connection(&self.pool).await?;
        connection
            .execute(sql, &[&definition.name.as_str(), &json.as_str()])
            .await
            .map_err(query_error)?;
        Ok(())
    }

    async fn load_schema_metadata(
        &self,
        name: &SchemaName,
    ) -> Result<Option<SchemaDefinition>, BackendError> {
        let sql = format!("SELECT [definition] FROM [dbo].[{METADATA}] WHERE [name] = @P1;");
        let mut connection = connection(&self.pool).await?;
        let rows = connection
            .query(sql, &[&name.as_str()])
            .await
            .map_err(query_error)?
            .into_first_result()
            .await
            .map_err(query_error)?;
        rows.first().map(definition_from_row).transpose()
    }

    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        let sql = format!("SELECT [definition] FROM [dbo].[{METADATA}] ORDER BY [name];");
        let mut connection = connection(&self.pool).await?;
        let rows = connection
            .simple_query(sql)
            .await
            .map_err(query_error)?
            .into_first_result()
            .await
            .map_err(query_error)?;
        rows.iter().map(definition_from_row).collect()
    }
}

impl EntityStore for MssqlBackend {
    async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let data = serde_json::to_string(&entity.fields).map_err(json_error)?;
        let sql = format!(
            "INSERT INTO {} ([id], [data]) VALUES (@P1, @P2);",
            quote(entity.schema.as_str())
        );
        let mut connection = connection(&self.pool).await?;
        connection
            .execute(sql, &[&entity.id.as_str(), &data.as_str()])
            .await
            .map_err(query_error)?;
        Ok(entity.clone())
    }

    async fn get(&self, schema: &SchemaName, id: &EntityId) -> Result<Entity, BackendError> {
        let sql = format!(
            "SELECT [id], [data] FROM {} WHERE [id] = @P1;",
            quote(schema.as_str())
        );
        let mut connection = connection(&self.pool).await?;
        let rows = connection
            .query(sql, &[&id.as_str()])
            .await
            .map_err(query_error)?
            .into_first_result()
            .await
            .map_err(query_error)?;
        rows.first()
            .map(|row| entity_from_row(row, schema))
            .transpose()?
            .ok_or_else(|| BackendError::EntityNotFound {
                schema: schema.to_string(),
                entity_id: id.to_string(),
            })
    }

    async fn update(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let data = serde_json::to_string(&entity.fields).map_err(json_error)?;
        let sql = format!(
            "UPDATE {} SET [data] = @P2 WHERE [id] = @P1;",
            quote(entity.schema.as_str())
        );
        let mut connection = connection(&self.pool).await?;
        let affected = connection
            .execute(sql, &[&entity.id.as_str(), &data.as_str()])
            .await
            .map_err(query_error)?
            .total();
        if affected == 0 {
            return Err(BackendError::EntityNotFound {
                schema: entity.schema.to_string(),
                entity_id: entity.id.to_string(),
            });
        }
        Ok(entity.clone())
    }

    async fn update_field_if_matches(
        &self,
        schema: &SchemaName,
        id: &EntityId,
        field: &schema_forge_core::types::FieldName,
        expected: &DynamicValue,
        value: &DynamicValue,
    ) -> Result<bool, BackendError> {
        if matches!(value, DynamicValue::Null)
            && self
                .load_schema_metadata(schema)
                .await?
                .and_then(|schema| schema.field(field.as_str()).cloned())
                .is_some_and(|field| field.is_required())
        {
            return Err(BackendError::RequiredFieldMissing {
                field: field.to_string(),
            });
        }
        let path = format!("$.\"{field}\"");
        let missing_matches = matches!(expected, DynamicValue::Null);
        let expected = serde_json::to_string(expected).map_err(json_error)?;
        let value = serde_json::to_string(value).map_err(json_error)?;
        let sql = format!("UPDATE {} SET [data] = JSON_MODIFY([data], @P2, JSON_QUERY(@P4)) WHERE [id] = @P1 AND (JSON_QUERY([data], @P2) COLLATE Latin1_General_100_BIN2 = @P3 OR (@P5 = 1 AND JSON_QUERY([data], @P2) IS NULL));", quote(schema.as_str()));
        let mut connection = connection(&self.pool).await?;
        let affected = connection
            .execute(
                sql,
                &[
                    &id.as_str(),
                    &path.as_str(),
                    &expected.as_str(),
                    &value.as_str(),
                    &missing_matches,
                ],
            )
            .await
            .map_err(query_error)?
            .total();
        Ok(affected != 0)
    }

    async fn delete(&self, schema: &SchemaName, id: &EntityId) -> Result<(), BackendError> {
        let sql = format!("DELETE FROM {} WHERE [id] = @P1;", quote(schema.as_str()));
        let mut connection = connection(&self.pool).await?;
        let affected = connection
            .execute(sql, &[&id.as_str()])
            .await
            .map_err(query_error)?
            .total();
        if affected == 0 {
            return Err(BackendError::EntityNotFound {
                schema: schema.to_string(),
                entity_id: id.to_string(),
            });
        }
        Ok(())
    }

    async fn query(&self, query: &Query) -> Result<QueryResult, BackendError> {
        let schema = self.schema_for_query(query).await?;
        let sql = format!(
            "SELECT [id], [data] FROM {} ORDER BY [id];",
            quote(schema.name.as_str())
        );
        let mut connection = connection(&self.pool).await?;
        let rows = connection
            .simple_query(sql)
            .await
            .map_err(query_error)?
            .into_first_result()
            .await
            .map_err(query_error)?;
        let mut entities: Vec<_> = rows
            .iter()
            .map(|row| entity_from_row(row, &schema.name))
            .collect::<Result<_, _>>()?;
        if let Some(filter) = &query.filter {
            entities.retain(|entity| matches_filter(entity, filter));
        }
        sort_entities(&mut entities, &query.sort);
        let total = query.include_total.then_some(entities.len());
        let offset = query.offset.unwrap_or(0).min(entities.len());
        let end = query.limit.map_or(entities.len(), |limit| {
            offset.saturating_add(limit).min(entities.len())
        });
        let mut entities = entities.drain(offset..end).collect::<Vec<_>>();
        if let Some(projection) = &query.projection {
            for entity in &mut entities {
                entity.fields.retain(|name, _| projection.contains(name));
            }
        }
        Ok(QueryResult::new(entities, total))
    }

    async fn count(&self, query: &Query) -> Result<usize, BackendError> {
        Ok(self
            .query(&Query {
                include_total: true,
                limit: None,
                offset: None,
                projection: None,
                ..query.clone()
            })
            .await?
            .total_count
            .unwrap_or(0))
    }

    async fn aggregate(
        &self,
        query: &AggregateQuery,
    ) -> Result<Vec<AggregateResult>, BackendError> {
        let schema = self
            .list_schema_metadata()
            .await?
            .into_iter()
            .find(|s| s.id == query.schema)
            .ok_or_else(|| BackendError::SchemaNotFound {
                schema: query.schema.to_string(),
            })?;
        let sql = format!("SELECT [id], [data] FROM {};", quote(schema.name.as_str()));
        let mut connection = connection(&self.pool).await?;
        let rows = connection
            .simple_query(sql)
            .await
            .map_err(query_error)?
            .into_first_result()
            .await
            .map_err(query_error)?;
        let mut entities = rows
            .iter()
            .map(|row| entity_from_row(row, &schema.name))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(filter) = &query.filter {
            entities.retain(|entity| matches_filter(entity, filter));
        }
        query
            .ops
            .iter()
            .map(|op| {
                aggregate_value(&entities, op).map(|value| AggregateResult {
                    op: op.clone(),
                    value,
                })
            })
            .collect()
    }
}

async fn connection(
    pool: &MssqlPool,
) -> Result<bb8::PooledConnection<'_, bb8_tiberius::ConnectionManager>, BackendError> {
    pool.get()
        .await
        .map_err(|error| BackendError::ConnectionError {
            message: error.to_string(),
        })
}

fn quote(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}
fn query_error(error: tiberius::error::Error) -> BackendError {
    BackendError::QueryError {
        message: error.to_string(),
    }
}
fn json_error(error: serde_json::Error) -> BackendError {
    BackendError::Internal {
        message: error.to_string(),
    }
}

fn definition_from_row(row: &tiberius::Row) -> Result<SchemaDefinition, BackendError> {
    let value: &str = row.get(0).ok_or_else(|| BackendError::Internal {
        message: "metadata row has no definition".into(),
    })?;
    serde_json::from_str(value).map_err(json_error)
}

fn entity_from_row(row: &tiberius::Row, schema: &SchemaName) -> Result<Entity, BackendError> {
    let id: &str = row.get(0).ok_or_else(|| BackendError::Internal {
        message: "entity row has no id".into(),
    })?;
    let data: &str = row.get(1).ok_or_else(|| BackendError::Internal {
        message: "entity row has no data".into(),
    })?;
    let id = EntityId::parse(id).map_err(|error| BackendError::Internal {
        message: error.to_string(),
    })?;
    let fields: BTreeMap<String, DynamicValue> = serde_json::from_str(data).map_err(json_error)?;
    Ok(Entity::with_id(id, schema.clone(), fields))
}

fn field_value<'a>(entity: &'a Entity, path: &FieldPath) -> Option<&'a DynamicValue> {
    let mut value = entity.fields.get(path.root())?;
    for segment in &path.segments()[1..] {
        value = match value {
            DynamicValue::Composite(values) | DynamicValue::Map(values) => values.get(segment)?,
            _ => return None,
        };
    }
    Some(value)
}

fn value_cmp(left: &DynamicValue, right: &DynamicValue) -> Option<Ordering> {
    match (left, right) {
        (DynamicValue::Null, DynamicValue::Null) => Some(Ordering::Equal),
        (DynamicValue::Null, _) => Some(Ordering::Less),
        (_, DynamicValue::Null) => Some(Ordering::Greater),
        (DynamicValue::Integer(a), DynamicValue::Integer(b)) => a.partial_cmp(b),
        (DynamicValue::Float(a), DynamicValue::Float(b)) => a.partial_cmp(b),
        (DynamicValue::Integer(a), DynamicValue::Float(b)) => (*a as f64).partial_cmp(b),
        (DynamicValue::Float(a), DynamicValue::Integer(b)) => a.partial_cmp(&(*b as f64)),
        (DynamicValue::Text(a), DynamicValue::Text(b))
        | (DynamicValue::Enum(a), DynamicValue::Enum(b)) => a.partial_cmp(b),
        (DynamicValue::Boolean(a), DynamicValue::Boolean(b)) => a.partial_cmp(b),
        (DynamicValue::DateTime(a), DynamicValue::DateTime(b)) => a.partial_cmp(b),
        (DynamicValue::Duration(a), DynamicValue::Duration(b)) => a.partial_cmp(b),
        _ => None,
    }
}

fn filter_value<'a>(entity: &'a Entity, path: &FieldPath) -> std::borrow::Cow<'a, DynamicValue> {
    if path.is_simple() && path.root() == "id" {
        std::borrow::Cow::Owned(DynamicValue::Text(entity.id.as_str().into()))
    } else {
        std::borrow::Cow::Borrowed(field_value(entity, path).unwrap_or(&DynamicValue::Null))
    }
}

fn matches_filter(entity: &Entity, filter: &Filter) -> bool {
    match filter {
        Filter::Eq { path, value } => filter_value(entity, path).as_ref() == value,
        Filter::Ne { path, value } => filter_value(entity, path).as_ref() != value,
        Filter::Gt { path, value } => {
            comparison_matches(entity, path, value, Ordering::Greater, false)
        }
        Filter::Gte { path, value } => {
            comparison_matches(entity, path, value, Ordering::Greater, true)
        }
        Filter::Lt { path, value } => {
            comparison_matches(entity, path, value, Ordering::Less, false)
        }
        Filter::Lte { path, value } => {
            comparison_matches(entity, path, value, Ordering::Less, true)
        }
        Filter::Contains { path, value } => {
            matches!(filter_value(entity, path).as_ref(), DynamicValue::Text(text) | DynamicValue::Enum(text) if text.contains(value))
        }
        Filter::StartsWith { path, value } => {
            matches!(filter_value(entity, path).as_ref(), DynamicValue::Text(text) | DynamicValue::Enum(text) if text.starts_with(value))
        }
        Filter::In { path, values } => values.contains(filter_value(entity, path).as_ref()),
        Filter::And { filters } => filters.iter().all(|filter| matches_filter(entity, filter)),
        Filter::Or { filters } => filters.iter().any(|filter| matches_filter(entity, filter)),
        Filter::Not { filter } => !matches_filter(entity, filter),
        _ => false,
    }
}

fn comparison_matches(
    entity: &Entity,
    path: &FieldPath,
    expected: &DynamicValue,
    ordering: Ordering,
    or_equal: bool,
) -> bool {
    value_cmp(filter_value(entity, path).as_ref(), expected)
        .is_some_and(|result| result == ordering || (or_equal && result == Ordering::Equal))
}

fn sort_entities(entities: &mut [Entity], sorts: &[(FieldPath, SortOrder)]) {
    entities.sort_by(|left, right| {
        for (path, order) in sorts {
            let ordering = if path.is_simple() && path.root() == "id" {
                left.id.as_str().cmp(right.id.as_str())
            } else {
                match (field_value(left, path), field_value(right, path)) {
                    (Some(a), Some(b)) => value_cmp(a, b).unwrap_or(Ordering::Equal),
                    (None, Some(_)) => Ordering::Less,
                    (Some(_), None) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }
            };
            let ordering = match order {
                SortOrder::Ascending => ordering,
                SortOrder::Descending => ordering.reverse(),
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        left.id.as_str().cmp(right.id.as_str())
    });
}

fn numeric(value: &DynamicValue) -> Option<f64> {
    match value {
        DynamicValue::Integer(value) => Some(*value as f64),
        DynamicValue::Float(value) => Some(*value),
        _ => None,
    }
}

fn aggregate_value(entities: &[Entity], op: &AggregateOp) -> Result<f64, BackendError> {
    match op {
        AggregateOp::Count => Ok(entities.len() as f64),
        AggregateOp::Sum { field } => Ok(entities
            .iter()
            .filter_map(|entity| field_value(entity, field).and_then(numeric))
            .sum()),
        AggregateOp::Avg { field } => {
            let values = entities
                .iter()
                .filter_map(|entity| field_value(entity, field).and_then(numeric))
                .collect::<Vec<_>>();
            Ok(if values.is_empty() {
                0.0
            } else {
                values.iter().sum::<f64>() / values.len() as f64
            })
        }
        _ => Err(BackendError::QueryError {
            message: "unsupported aggregate operation".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use schema_forge_backend::Entity;
    use schema_forge_core::query::{AggregateOp, FieldPath, Filter, SortOrder};
    use schema_forge_core::types::{DynamicValue, SchemaName};

    use super::{aggregate_value, matches_filter, quote, sort_entities};

    fn entity(name: &str, score: i64) -> Entity {
        Entity::new(
            SchemaName::new("Player").unwrap(),
            BTreeMap::from([
                ("name".into(), DynamicValue::Text(name.into())),
                ("score".into(), DynamicValue::Integer(score)),
            ]),
        )
    }

    #[test]
    fn sql_server_identifiers_escape_brackets() {
        assert_eq!(quote("a]b"), "[a]]b]");
    }

    #[test]
    fn document_filters_and_sorts_match_backend_semantics() {
        let filter = Filter::and(vec![
            Filter::starts_with(FieldPath::single("name"), "A"),
            Filter::gte(FieldPath::single("score"), DynamicValue::Integer(10)),
        ]);
        assert!(matches_filter(&entity("Alice", 12), &filter));
        assert!(!matches_filter(&entity("Bob", 12), &filter));

        let mut entities = vec![entity("low", 1), entity("high", 9)];
        sort_entities(
            &mut entities,
            &[(FieldPath::single("score"), SortOrder::Descending)],
        );
        assert_eq!(entities[0].field("score"), Some(&DynamicValue::Integer(9)));
    }

    #[test]
    fn numeric_aggregates_support_integer_and_float_fields() {
        let entities = vec![entity("one", 10), entity("two", 20)];
        let field = FieldPath::single("score");
        assert_eq!(
            aggregate_value(&entities, &AggregateOp::Count).unwrap(),
            2.0
        );
        assert_eq!(
            aggregate_value(
                &entities,
                &AggregateOp::Sum {
                    field: field.clone()
                }
            )
            .unwrap(),
            30.0
        );
        assert_eq!(
            aggregate_value(&entities, &AggregateOp::Avg { field }).unwrap(),
            15.0
        );
    }
}
