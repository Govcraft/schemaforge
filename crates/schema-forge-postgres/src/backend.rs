//! PostgreSQL implementation of the `SchemaBackend` and `EntityStore` traits.
//!
//! This is the I/O boundary: all database communication happens here.
//! Pure logic lives in `codegen`, `query`, and `value` modules.

use std::sync::Arc;

use arc_swap::ArcSwap;
use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::traits::{EntityStore, SchemaBackend};
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{
    DynamicValue, EntityId, FieldType, SchemaDefinition, SchemaName, WidgetRepair,
};
use sqlx::postgres::{PgArguments, PgPool, PgPoolOptions, PgRow};
use sqlx::{Arguments, Row};

use crate::codegen::migration_step_to_sql;
use crate::query::{aggregate_to_sql, count_to_sql, query_to_sql};
use crate::value::{bind_dynamic_value, row_to_entity};

/// The schema metadata table name used to store `SchemaDefinition` records.
const SCHEMA_META_TABLE: &str = "_schema_metadata";

/// Emit a tracing warning for each legacy widget annotation repaired at
/// metadata load time. Noisy by design — operators should see every stale
/// row they need to clean up.
fn log_widget_repairs(schema: &str, repairs: Vec<WidgetRepair>) {
    for repair in repairs {
        match repair {
            WidgetRepair::Remapped { from, to } => {
                tracing::warn!(
                    schema,
                    from = %from,
                    to = %to,
                    "schema metadata: remapped legacy @widget token; rerun `schemaforge apply` to persist the fix"
                );
            }
            WidgetRepair::Dropped { token } => {
                tracing::warn!(
                    schema,
                    token = %token,
                    "schema metadata: dropped unknown @widget token; rerun `schemaforge apply` to persist the fix"
                );
            }
        }
    }
}

/// PostgreSQL backend for SchemaForge.
///
/// Wraps a `PgPool` and implements both `SchemaBackend` (DDL/metadata)
/// and `EntityStore` (CRUD/query).
///
/// Holds an in-memory cache of the `_schema_metadata` table so that the
/// hot `query()` / `count()` / `aggregate()` paths can resolve a schema
/// by id without hitting the database on every call. The cache is
/// populated lazily on first read and invalidated whenever this backend
/// mutates the metadata table (store/delete/apply_migration).
///
/// The cache is a single-writer cache: it only reflects mutations made
/// through this backend instance. External writers to `_schema_metadata`
/// are not observed — but SchemaForge is a single-writer service, so
/// this is not a concern in practice.
pub struct PgBackend {
    pool: PgPool,
    schema_cache: ArcSwap<Option<Arc<Vec<SchemaDefinition>>>>,
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

        let backend = Self::from_parts(pool);
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

        let backend = Self::from_parts(pool);
        backend.ensure_metadata_table().await?;
        Ok(backend)
    }

    /// Create a backend from an existing connection pool.
    pub async fn from_pool(pool: PgPool) -> Result<Self, BackendError> {
        let backend = Self::from_parts(pool);
        backend.ensure_metadata_table().await?;
        Ok(backend)
    }

    fn from_parts(pool: PgPool) -> Self {
        Self {
            pool,
            schema_cache: ArcSwap::new(Arc::new(None)),
        }
    }

    /// Drop the cached schema metadata so the next read refetches from
    /// the database. Called from every mutation path that changes the
    /// `_schema_metadata` table.
    fn invalidate_schema_cache(&self) {
        self.schema_cache.store(Arc::new(None));
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
    ///
    /// When `schema_def` is provided, per-column `FieldType` context is passed
    /// to `bind_dynamic_value` so array columns can be bound as native Postgres
    /// arrays (`text[]`, `bigint[]`, etc.) rather than JSONB.
    fn build_insert(
        entity: &Entity,
        schema_def: Option<&SchemaDefinition>,
    ) -> Result<(String, PgArguments), BackendError> {
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
            let field_type = schema_def
                .and_then(|sd| sd.field(col))
                .map(|fd| &fd.field_type);
            bind_dynamic_value(&mut args, val, field_type)?;
        }

        let sql = format!(
            "INSERT INTO \"{table}\" ({}) VALUES ({}) RETURNING *;",
            columns.join(", "),
            placeholders.join(", ")
        );

        Ok((sql, args))
    }

    /// Build a parameterized UPDATE statement and arguments for an entity.
    ///
    /// When `schema_def` is provided, per-column `FieldType` context is passed
    /// to `bind_dynamic_value` so array columns can be bound as native Postgres
    /// arrays (`text[]`, `bigint[]`, etc.) rather than JSONB.
    fn build_update(
        entity: &Entity,
        schema_def: Option<&SchemaDefinition>,
    ) -> Result<(String, PgArguments), BackendError> {
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
            let field_type = schema_def
                .and_then(|sd| sd.field(col))
                .map(|fd| &fd.field_type);
            bind_dynamic_value(&mut args, val, field_type)?;
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
        // Hot path: read the cached schema list (populated lazily by
        // cached_schema_list) and clone out the single matching entry.
        // Falls through to a fresh DB read on cache miss.
        let cached = self.cached_schema_list().await?;
        cached
            .iter()
            .find(|s| s.id == *schema_id)
            .cloned()
            .ok_or_else(|| BackendError::QueryError {
                message: format!("no schema found for id '{}'", schema_id.as_str()),
            })
    }

    /// Return the cached `_schema_metadata` snapshot, fetching from the
    /// database on the first call after startup or after any invalidation.
    /// The returned `Arc<Vec<...>>` can be shared cheaply across tasks.
    async fn cached_schema_list(&self) -> Result<Arc<Vec<SchemaDefinition>>, BackendError> {
        let current = self.schema_cache.load_full();
        if let Some(list) = current.as_ref() {
            return Ok(list.clone());
        }
        // Cache miss — hit the DB, build the Arc, install it, return it.
        // It's fine if two concurrent cache misses both do this; the
        // store() resolves to whichever lands last, and both callers see
        // consistent data.
        let fresh = self.fetch_schema_metadata_from_db().await?;
        let shared = Arc::new(fresh);
        self.schema_cache.store(Arc::new(Some(shared.clone())));
        Ok(shared)
    }

    async fn fetch_schema_metadata_from_db(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
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
            let mut json: serde_json::Value =
                row.try_get("definition")
                    .map_err(|e| BackendError::Internal {
                        message: format!("failed to read definition column: {e}"),
                    })?;
            let schema_label = json
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>")
                .to_string();
            log_widget_repairs(
                &schema_label,
                schema_forge_core::types::sanitize_schema_metadata_json(&mut json),
            );
            let definition: SchemaDefinition =
                serde_json::from_value(json).map_err(|e| BackendError::Internal {
                    message: format!("failed to deserialize schema metadata: {e}"),
                })?;
            definitions.push(definition);
        }

        Ok(definitions)
    }

    /// Bind compiled query parameters into PgArguments.
    ///
    /// Compiled query parameters are positional and not tied to a specific
    /// column, so array parameters fall back to JSONB binding. This is
    /// acceptable today because compiled query WHERE-clause predicates do not
    /// currently bind array literals; native-array binding is only needed for
    /// column writes (INSERT/UPDATE), which go through `build_insert` /
    /// `build_update` with schema context.
    /// Re-type legacy `NUMERIC` columns produced by early Postgres codegen
    /// back to `DOUBLE PRECISION` for any `Float` field in `definition`.
    ///
    /// Introspects `information_schema.columns` for the schema's table and
    /// issues `ALTER TABLE ... ALTER COLUMN ... TYPE DOUBLE PRECISION
    /// USING "col"::double precision` for each Float column whose live
    /// type is `numeric`. Columns that don't exist yet (the table may be
    /// created by the subsequent migration), or that already live in
    /// `double precision`, are skipped. See GH #37.
    async fn repair_float_columns(
        &self,
        definition: &SchemaDefinition,
    ) -> Result<(), BackendError> {
        let float_columns: Vec<&str> = definition
            .fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Float(_)))
            .map(|f| f.name.as_str())
            .collect();
        if float_columns.is_empty() {
            return Ok(());
        }

        let table = definition.name.as_str();
        // Pull every column's live data_type for this table in a single
        // round-trip, then filter in Rust. Using a plain scalar comparison
        // avoids threading a text[] parameter through sqlx.
        let rows = sqlx::query(
            "SELECT column_name, data_type \
             FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1;",
        )
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| BackendError::QueryError {
            message: format!("failed to introspect columns for '{table}': {e}"),
        })?;

        for row in rows {
            let col: String = row.try_get("column_name").map_err(|e| BackendError::Internal {
                message: format!("failed to read column_name: {e}"),
            })?;
            if !float_columns.contains(&col.as_str()) {
                continue;
            }
            let data_type: String = row.try_get("data_type").map_err(|e| BackendError::Internal {
                message: format!("failed to read data_type: {e}"),
            })?;
            if data_type != "numeric" {
                continue;
            }
            tracing::warn!(
                schema = table,
                column = %col,
                "repairing legacy NUMERIC column to DOUBLE PRECISION (GH #37)"
            );
            let alter = format!(
                "ALTER TABLE \"{table}\" ALTER COLUMN \"{col}\" TYPE DOUBLE PRECISION USING \"{col}\"::double precision;"
            );
            sqlx::query(&alter).execute(&self.pool).await.map_err(|e| {
                BackendError::MigrationFailed {
                    step: format!("repair_float_column({table}.{col})"),
                    reason: e.to_string(),
                }
            })?;
        }

        Ok(())
    }

    fn bind_params(params: &[DynamicValue]) -> Result<PgArguments, BackendError> {
        let mut args = PgArguments::default();
        for param in params {
            bind_dynamic_value(&mut args, param, None)?;
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
                sqlx::query(stmt).execute(&mut *tx).await.map_err(|e| {
                    BackendError::MigrationFailed {
                        step: step.to_string(),
                        reason: e.to_string(),
                    }
                })?;
            }
        }

        tx.commit()
            .await
            .map_err(|e| BackendError::MigrationFailed {
                step: "commit transaction".to_string(),
                reason: e.to_string(),
            })?;

        // Migrations rewrite entity-table DDL; they don't directly touch
        // `_schema_metadata`, but the schema metadata usually gets
        // store()'d right after. Drop the cache now so the next reader
        // sees fresh data even if only a migration ran.
        self.invalidate_schema_cache();

        Ok(())
    }

    async fn store_schema_metadata(
        &self,
        definition: &SchemaDefinition,
    ) -> Result<(), BackendError> {
        // Legacy repair: early builds of the Postgres codegen emitted
        // `NUMERIC(p,0)` for `float(precision: N)` fields. The current read
        // path decodes these columns as `FLOAT8`, which sqlx refuses to do
        // against NUMERIC, 500'ing every GET/PUT on rows with a written
        // float value. Detect and silently re-type any such columns to
        // DOUBLE PRECISION whenever the schema is (re)stored — this is
        // the canonical "schema is authoritative" checkpoint. See GH #37.
        self.repair_float_columns(definition).await?;

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

        self.invalidate_schema_cache();

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
                let mut json: serde_json::Value =
                    row.try_get("definition")
                        .map_err(|e| BackendError::Internal {
                            message: format!("failed to read definition column: {e}"),
                        })?;
                log_widget_repairs(
                    name_str,
                    schema_forge_core::types::sanitize_schema_metadata_json(&mut json),
                );
                let definition: SchemaDefinition =
                    serde_json::from_value(json).map_err(|e| BackendError::Internal {
                        message: format!("failed to deserialize schema metadata: {e}"),
                    })?;
                Ok(Some(definition))
            }
        }
    }

    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        // Reads go through the in-memory cache. The trait returns an owned
        // Vec, so we clone the cached Arc<Vec> contents once here; hot paths
        // that can tolerate a shared Arc should call `cached_schema_list`
        // directly to avoid this copy.
        let cached = self.cached_schema_list().await?;
        Ok((*cached).clone())
    }
}

impl EntityStore for PgBackend {
    async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let schema_def = self.load_schema_metadata(&entity.schema).await?;
        let (sql, args) = Self::build_insert(entity, schema_def.as_ref())?;

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
        let (sql, args) = Self::build_update(entity, schema_def.as_ref())?;

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

        sqlx::query(&format!("DELETE FROM \"{table}\" WHERE \"id\" = $1;"))
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

        // When `include_total` is set, compute the SELECT and the COUNT(*)
        // in parallel on two pool connections instead of sequentially.
        // Each sqlx round-trip costs ~2 * network RTT (Parse/Execute), so
        // running them concurrently halves wall-clock DB time for list
        // endpoints that need a `total_count`.
        let main_fut = sqlx::query_with(&compiled.sql, args).fetch_all(&self.pool);

        let rows: Vec<PgRow> = if query.include_total {
            let count_compiled = count_to_sql(query, table);
            let count_args = Self::bind_params(&count_compiled.params)?;
            let count_fut = sqlx::query_with(&count_compiled.sql, count_args)
                .fetch_one(&self.pool);

            let (rows_res, count_res) = tokio::join!(main_fut, count_fut);
            let rows = rows_res.map_err(|e| BackendError::QueryError {
                message: format!("failed to execute query: {e}"),
            })?;
            let count_row = count_res.map_err(|e| BackendError::QueryError {
                message: format!("failed to execute count query: {e}"),
            })?;
            let total: i64 = count_row
                .try_get("count")
                .map_err(|e| BackendError::Internal {
                    message: format!("failed to read count: {e}"),
                })?;
            let schema_name = schema_def.name.clone();
            let mut entities = Vec::new();
            for row in &rows {
                entities.push(row_to_entity(row, &schema_name, Some(&schema_def))?);
            }
            return Ok(QueryResult::new(entities, Some(total as usize)));
        } else {
            main_fut.await.map_err(|e| BackendError::QueryError {
                message: format!("failed to execute query: {e}"),
            })?
        };

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
                        .or_else(|_| row.try_get::<i64, _>(key.as_str()).map(|v| v as f64))
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
