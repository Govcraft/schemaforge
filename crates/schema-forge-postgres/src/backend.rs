//! PostgreSQL implementation of the `SchemaBackend` and `EntityStore` traits.
//!
//! This is the I/O boundary: all database communication happens here.
//! Pure logic lives in `codegen`, `query`, and `value` modules.

use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::traits::{EntityStore, SchemaBackend};
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition, SchemaName};
use sqlx::postgres::{PgArguments, PgPool, PgPoolOptions, PgRow};
use sqlx::{Arguments, Row};

use crate::codegen::migration_step_to_sql;
use crate::query::{aggregate_to_sql, count_to_sql, query_to_sql};
use crate::value::{bind_dynamic_value, row_to_entity};

/// The schema metadata table name used to store `SchemaDefinition` records.
const SCHEMA_META_TABLE: &str = "_schema_metadata";

/// PostgreSQL backend for SchemaForge.
///
/// Wraps a `PgPool` and implements both `SchemaBackend` (DDL/metadata)
/// and `EntityStore` (CRUD/query).
pub struct PgBackend {
    pool: PgPool,
}

impl PgBackend {
    /// Connect to a PostgreSQL instance using a connection URL.
    ///
    /// Supports `postgres://` and `postgresql://` URL schemes.
    /// Creates a connection pool with sensible defaults.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("failed to connect to PostgreSQL at {url}: {e}"),
            })?;

        let backend = Self { pool };
        backend.ensure_metadata_table().await?;
        Ok(backend)
    }

    /// Connect with a custom maximum connection count.
    pub async fn connect_with_max_connections(
        url: &str,
        max_connections: u32,
    ) -> Result<Self, BackendError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("failed to connect to PostgreSQL at {url}: {e}"),
            })?;

        let backend = Self { pool };
        backend.ensure_metadata_table().await?;
        Ok(backend)
    }

    /// Create a backend from an existing connection pool.
    pub async fn from_pool(pool: PgPool) -> Result<Self, BackendError> {
        let backend = Self { pool };
        backend.ensure_metadata_table().await?;
        Ok(backend)
    }

    /// Get a reference to the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Ensure the `_schema_metadata` table exists.
    async fn ensure_metadata_table(&self) -> Result<(), BackendError> {
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS \"{SCHEMA_META_TABLE}\" (\
                \"name\" TEXT PRIMARY KEY, \
                \"definition\" JSONB NOT NULL\
            );"
        ))
        .execute(&self.pool)
        .await
        .map_err(|e| BackendError::MigrationFailed {
            step: "create _schema_metadata table".to_string(),
            reason: e.to_string(),
        })?;
        Ok(())
    }

    /// Build a parameterized INSERT statement and arguments for an entity.
    fn build_insert(entity: &Entity) -> Result<(String, PgArguments), BackendError> {
        let table = entity.schema.as_str();
        let mut columns = vec!["\"id\"".to_string()];
        let mut placeholders = vec!["$1".to_string()];
        let mut args = PgArguments::default();

        args.add(entity.id.as_str())
            .map_err(|e| BackendError::Internal {
                message: format!("failed to bind id: {e}"),
            })?;

        for (i, (col, val)) in entity.fields.iter().enumerate() {
            columns.push(format!("\"{col}\""));
            placeholders.push(format!("${}", i + 2));
            bind_dynamic_value(&mut args, val)?;
        }

        let sql = format!(
            "INSERT INTO \"{table}\" ({}) VALUES ({}) RETURNING *;",
            columns.join(", "),
            placeholders.join(", ")
        );

        Ok((sql, args))
    }

    /// Build a parameterized UPDATE statement and arguments for an entity.
    fn build_update(entity: &Entity) -> Result<(String, PgArguments), BackendError> {
        let table = entity.schema.as_str();
        let mut args = PgArguments::default();

        // $1 = id
        args.add(entity.id.as_str())
            .map_err(|e| BackendError::Internal {
                message: format!("failed to bind id: {e}"),
            })?;

        let mut set_clauses = Vec::new();
        for (i, (col, val)) in entity.fields.iter().enumerate() {
            set_clauses.push(format!("\"{col}\" = ${}", i + 2));
            bind_dynamic_value(&mut args, val)?;
        }

        let sql = format!(
            "UPDATE \"{table}\" SET {} WHERE \"id\" = $1 RETURNING *;",
            set_clauses.join(", ")
        );

        Ok((sql, args))
    }

    /// Look up the schema definition for a query's SchemaId.
    async fn resolve_schema_for_query(
        &self,
        schema_id: &schema_forge_core::types::SchemaId,
    ) -> Result<SchemaDefinition, BackendError> {
        let all_schemas = self.list_schema_metadata().await?;
        all_schemas
            .into_iter()
            .find(|s| s.id == *schema_id)
            .ok_or_else(|| BackendError::QueryError {
                message: format!("no schema found for id '{}'", schema_id.as_str()),
            })
    }

    /// Bind compiled query parameters into PgArguments.
    fn bind_params(params: &[DynamicValue]) -> Result<PgArguments, BackendError> {
        let mut args = PgArguments::default();
        for param in params {
            bind_dynamic_value(&mut args, param)?;
        }
        Ok(args)
    }
}

impl SchemaBackend for PgBackend {
    async fn apply_migration(
        &self,
        schema_name: &SchemaName,
        steps: &[MigrationStep],
    ) -> Result<(), BackendError> {
        let table = schema_name.as_str();

        // Execute all steps in a transaction for atomicity
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("failed to begin transaction: {e}"),
            })?;

        for step in steps {
            let statements = migration_step_to_sql(table, step);
            for stmt in &statements {
                sqlx::query(stmt)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| BackendError::MigrationFailed {
                        step: step.to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        tx.commit()
            .await
            .map_err(|e| BackendError::MigrationFailed {
                step: "commit transaction".to_string(),
                reason: e.to_string(),
            })?;

        Ok(())
    }

    async fn store_schema_metadata(
        &self,
        definition: &SchemaDefinition,
    ) -> Result<(), BackendError> {
        let json = serde_json::to_value(definition).map_err(|e| BackendError::Internal {
            message: format!("failed to serialize schema metadata: {e}"),
        })?;

        let name = definition.name.as_str();
        sqlx::query(&format!(
            "INSERT INTO \"{SCHEMA_META_TABLE}\" (\"name\", \"definition\") \
             VALUES ($1, $2) \
             ON CONFLICT (\"name\") DO UPDATE SET \"definition\" = $2;"
        ))
        .bind(name)
        .bind(&json)
        .execute(&self.pool)
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("failed to store schema metadata: {e}"),
        })?;

        Ok(())
    }

    async fn load_schema_metadata(
        &self,
        name: &SchemaName,
    ) -> Result<Option<SchemaDefinition>, BackendError> {
        let name_str = name.as_str();
        let row: Option<PgRow> = sqlx::query(&format!(
            "SELECT \"definition\" FROM \"{SCHEMA_META_TABLE}\" WHERE \"name\" = $1;"
        ))
        .bind(name_str)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("failed to load schema metadata: {e}"),
        })?;

        match row {
            None => Ok(None),
            Some(row) => {
                let json: serde_json::Value =
                    row.try_get("definition").map_err(|e| BackendError::Internal {
                        message: format!("failed to read definition column: {e}"),
                    })?;
                let definition: SchemaDefinition =
                    serde_json::from_value(json).map_err(|e| BackendError::Internal {
                        message: format!("failed to deserialize schema metadata: {e}"),
                    })?;
                Ok(Some(definition))
            }
        }
    }

    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        let rows: Vec<PgRow> = sqlx::query(&format!(
            "SELECT \"definition\" FROM \"{SCHEMA_META_TABLE}\";"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("failed to list schema metadata: {e}"),
        })?;

        let mut definitions = Vec::new();
        for row in &rows {
            let json: serde_json::Value =
                row.try_get("definition").map_err(|e| BackendError::Internal {
                    message: format!("failed to read definition column: {e}"),
                })?;
            let definition: SchemaDefinition =
                serde_json::from_value(json).map_err(|e| BackendError::Internal {
                    message: format!("failed to deserialize schema metadata: {e}"),
                })?;
            definitions.push(definition);
        }

        Ok(definitions)
    }
}

impl EntityStore for PgBackend {
    async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let schema_def = self.load_schema_metadata(&entity.schema).await?;
        let (sql, args) = Self::build_insert(entity)?;

        let row: PgRow = sqlx::query_with(&sql, args)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("failed to create entity: {e}"),
            })?;

        row_to_entity(&row, &entity.schema, schema_def.as_ref())
    }

    async fn get(&self, schema: &SchemaName, id: &EntityId) -> Result<Entity, BackendError> {
        let schema_def = self.load_schema_metadata(schema).await?;
        let table = schema.as_str();
        let sql = format!("SELECT * FROM \"{table}\" WHERE \"id\" = $1;");

        let row: Option<PgRow> = sqlx::query(&sql)
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("failed to get entity: {e}"),
            })?;

        match row {
            None => Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id.as_str().to_string(),
            }),
            Some(row) => row_to_entity(&row, schema, schema_def.as_ref()),
        }
    }

    async fn update(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let schema_def = self.load_schema_metadata(&entity.schema).await?;
        let (sql, args) = Self::build_update(entity)?;

        let row: Option<PgRow> = sqlx::query_with(&sql, args)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("failed to update entity: {e}"),
            })?;

        match row {
            None => Err(BackendError::EntityNotFound {
                schema: entity.schema.as_str().to_string(),
                entity_id: entity.id.as_str().to_string(),
            }),
            Some(row) => row_to_entity(&row, &entity.schema, schema_def.as_ref()),
        }
    }

    async fn delete(&self, schema: &SchemaName, id: &EntityId) -> Result<(), BackendError> {
        let table = schema.as_str();

        // Check existence first
        let exists: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM \"{table}\" WHERE \"id\" = $1);"
        ))
        .bind(id.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("failed to check entity existence: {e}"),
        })?;

        if !exists {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id.as_str().to_string(),
            });
        }

        sqlx::query(&format!(
            "DELETE FROM \"{table}\" WHERE \"id\" = $1;"
        ))
        .bind(id.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("failed to delete entity: {e}"),
        })?;

        Ok(())
    }

    async fn query(&self, query: &Query) -> Result<QueryResult, BackendError> {
        let schema_def = self.resolve_schema_for_query(&query.schema).await?;
        let table = schema_def.name.as_str();
        let compiled = query_to_sql(query, table);
        let args = Self::bind_params(&compiled.params)?;

        let rows: Vec<PgRow> = sqlx::query_with(&compiled.sql, args)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("failed to execute query: {e}"),
            })?;

        let schema_name = schema_def.name.clone();
        let mut entities = Vec::new();
        for row in &rows {
            entities.push(row_to_entity(row, &schema_name, Some(&schema_def))?);
        }

        Ok(QueryResult::new(entities, None))
    }

    async fn count(&self, query: &Query) -> Result<usize, BackendError> {
        let schema_def = self.resolve_schema_for_query(&query.schema).await?;
        let table = schema_def.name.as_str();
        let compiled = count_to_sql(query, table);
        let args = Self::bind_params(&compiled.params)?;

        let row: PgRow = sqlx::query_with(&compiled.sql, args)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("failed to execute count query: {e}"),
            })?;

        let count: i64 = row.try_get("count").map_err(|e| BackendError::Internal {
            message: format!("failed to read count: {e}"),
        })?;

        Ok(count as usize)
    }

    async fn aggregate(
        &self,
        query: &AggregateQuery,
    ) -> Result<Vec<AggregateResult>, BackendError> {
        let schema_def = self.resolve_schema_for_query(&query.schema).await?;
        let table = schema_def.name.as_str();
        let compiled = aggregate_to_sql(query, table);
        let args = Self::bind_params(&compiled.params)?;

        let row: Option<PgRow> = sqlx::query_with(&compiled.sql, args)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| BackendError::QueryError {
                message: format!("failed to execute aggregate query: {e}"),
            })?;

        let mut results = Vec::with_capacity(query.ops.len());
        match row {
            Some(row) => {
                for (i, op) in query.ops.iter().enumerate() {
                    let key = format!("agg_{i}");
                    let value: f64 = row
                        .try_get::<f64, _>(key.as_str())
                        .or_else(|_| {
                            row.try_get::<i64, _>(key.as_str()).map(|v| v as f64)
                        })
                        .unwrap_or(0.0);
                    results.push(AggregateResult {
                        op: op.clone(),
                        value,
                    });
                }
            }
            None => {
                for op in &query.ops {
                    results.push(AggregateResult {
                        op: op.clone(),
                        value: 0.0,
                    });
                }
            }
        }

        Ok(results)
    }
}
