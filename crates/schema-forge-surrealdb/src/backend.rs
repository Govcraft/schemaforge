//! SurrealDB implementation of the `SchemaBackend` and `EntityStore` traits.
//!
//! This is the I/O boundary: all database communication happens here.
//! Pure logic lives in `codegen`, `query`, and `value` modules.

use std::collections::BTreeMap;

use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::traits::{EntityStore, SchemaBackend};
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::Query;
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};
use surrealdb::engine::any::Any;
use surrealdb::Surreal;

use crate::codegen::migration_step_to_surql;
use crate::query::query_to_surql;
use crate::value::{entity_to_surreal_map, surreal_to_dynamic};

/// The schema metadata table name used to store `SchemaDefinition` records.
const SCHEMA_META_TABLE: &str = "_schema_metadata";

/// SurrealDB backend for SchemaForge.
///
/// Wraps a connected `Surreal<Any>` client and implements both
/// `SchemaBackend` (DDL/metadata) and `EntityStore` (CRUD/query).
pub struct SurrealBackend {
    db: Surreal<Any>,
}

impl SurrealBackend {
    /// Create a SurrealBackend from an existing connected client.
    ///
    /// Use this when the connection is managed externally (e.g., by acton-service's
    /// connection pooling). The caller is responsible for ensuring the client
    /// has the correct namespace and database selected.
    pub fn from_client(db: Surreal<Any>) -> Self {
        Self { db }
    }

    /// Connect to an in-memory SurrealDB instance for testing.
    ///
    /// Uses the `kv-mem` engine. The namespace and database are created
    /// automatically.
    pub async fn connect_memory(ns: &str, db_name: &str) -> Result<Self, BackendError> {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: e.to_string(),
            })?;

        db.use_ns(ns)
            .use_db(db_name)
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: e.to_string(),
            })?;

        Ok(Self { db })
    }

    /// Connect to a remote SurrealDB instance.
    ///
    /// Supports ws://, wss://, http://, https://, and mem:// schemes.
    /// After connecting, selects the given namespace and database.
    pub async fn connect(url: &str, ns: &str, db_name: &str) -> Result<Self, BackendError> {
        let db = surrealdb::engine::any::connect(url)
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("failed to connect to {url}: {e}"),
            })?;

        db.use_ns(ns)
            .use_db(db_name)
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("failed to select namespace/database: {e}"),
            })?;

        Ok(Self { db })
    }

    /// Execute a raw SurrealQL statement, returning the response.
    async fn execute_raw(&self, sql: &str) -> Result<surrealdb::Response, BackendError> {
        self.db
            .query(sql)
            .await
            .map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })
    }

    /// Execute a raw SurrealQL statement and extract the result as a list of
    /// `surrealdb::sql::Value` objects.
    ///
    /// Uses `response.take::<surrealdb::Value>(0)` which bypasses serde
    /// deserialization (the SDK has a special `QueryResult<Value>` impl that
    /// wraps the core value directly). We then unwrap the `Array` variant
    /// manually to get individual row values.
    async fn execute_and_take_rows(
        &self,
        sql: &str,
    ) -> Result<Vec<surrealdb::sql::Value>, BackendError> {
        let mut response = self.execute_raw(sql).await?;
        let value: surrealdb::Value = response.take(0).map_err(|e| BackendError::QueryError {
            message: e.to_string(),
        })?;
        let core_val = value.into_inner();
        match core_val {
            surrealdb::sql::Value::Array(arr) => Ok(arr.0),
            surrealdb::sql::Value::None | surrealdb::sql::Value::Null => Ok(Vec::new()),
            // Single object result (e.g. from CREATE)
            other => Ok(vec![other]),
        }
    }
}

impl SchemaBackend for SurrealBackend {
    async fn apply_migration(
        &self,
        schema_name: &SchemaName,
        steps: &[MigrationStep],
    ) -> Result<(), BackendError> {
        let table = schema_name.as_str();
        for step in steps {
            let statements = migration_step_to_surql(table, step);
            for stmt in &statements {
                self.execute_raw(stmt)
                    .await
                    .map_err(|e| BackendError::MigrationFailed {
                        step: step.to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }
        Ok(())
    }

    async fn store_schema_metadata(
        &self,
        definition: &SchemaDefinition,
    ) -> Result<(), BackendError> {
        let json = serde_json::to_string(definition).map_err(|e| BackendError::Internal {
            message: format!("failed to serialize schema metadata: {e}"),
        })?;

        let name = definition.name.as_str();
        let sql = format!(
            "UPSERT {SCHEMA_META_TABLE}:`{name}` CONTENT {{ name: '{name}', definition: '{json_escaped}' }};",
            json_escaped = json.replace('\'', "\\'")
        );
        self.execute_raw(&sql).await?;
        Ok(())
    }

    async fn load_schema_metadata(
        &self,
        name: &SchemaName,
    ) -> Result<Option<SchemaDefinition>, BackendError> {
        let name_str = name.as_str();
        let sql = format!("SELECT definition FROM {SCHEMA_META_TABLE}:`{name_str}`;");
        let mut response = self.execute_raw(&sql).await?;

        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            return Ok(None);
        }

        let def_str = rows[0]
            .get("definition")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackendError::Internal {
                message: "schema metadata record missing 'definition' field".to_string(),
            })?;

        let definition: SchemaDefinition =
            serde_json::from_str(def_str).map_err(|e| BackendError::Internal {
                message: format!("failed to deserialize schema metadata: {e}"),
            })?;

        Ok(Some(definition))
    }

    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        let sql = format!("SELECT definition FROM {SCHEMA_META_TABLE};");
        let mut response = self.execute_raw(&sql).await?;

        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

        let mut definitions = Vec::new();
        for row in &rows {
            let def_str = row
                .get("definition")
                .and_then(|v| v.as_str())
                .ok_or_else(|| BackendError::Internal {
                    message: "schema metadata record missing 'definition' field".to_string(),
                })?;

            let definition: SchemaDefinition =
                serde_json::from_str(def_str).map_err(|e| BackendError::Internal {
                    message: format!("failed to deserialize schema metadata: {e}"),
                })?;
            definitions.push(definition);
        }

        Ok(definitions)
    }
}

impl EntityStore for SurrealBackend {
    async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let table = entity.schema.as_str();
        let id_str = entity.id.as_str();

        // Build field assignments
        let field_map = entity_to_surreal_map(entity);
        let mut assignments = Vec::new();
        for (k, v) in &field_map {
            if k == "id" {
                continue;
            }
            let literal = field_surreal_value_to_literal(v);
            assignments.push(format!("{k} = {literal}"));
        }

        let set_clause = assignments.join(", ");
        let sql = format!("CREATE {table}:`{id_str}` SET {set_clause};");

        let rows = self.execute_and_take_rows(&sql).await?;

        if rows.is_empty() {
            return Err(BackendError::Internal {
                message: format!("CREATE returned no result for {table}:{id_str}"),
            });
        }

        surreal_row_to_entity(&entity.schema, &rows[0])
    }

    async fn get(&self, schema: &SchemaName, id: &EntityId) -> Result<Entity, BackendError> {
        let table = schema.as_str();
        let id_str = id.as_str();
        let sql = format!("SELECT * FROM {table}:`{id_str}`;");

        let rows = self.execute_and_take_rows(&sql).await?;

        if rows.is_empty() {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id_str.to_string(),
            });
        }

        surreal_row_to_entity(schema, &rows[0])
    }

    async fn update(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let table = entity.schema.as_str();
        let id_str = entity.id.as_str();

        let field_map = entity_to_surreal_map(entity);
        let mut assignments = Vec::new();
        for (k, v) in &field_map {
            if k == "id" {
                continue;
            }
            let literal = field_surreal_value_to_literal(v);
            assignments.push(format!("{k} = {literal}"));
        }

        let set_clause = assignments.join(", ");
        let sql = format!("UPDATE {table}:`{id_str}` SET {set_clause};");

        let rows = self.execute_and_take_rows(&sql).await?;

        if rows.is_empty() {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id_str.to_string(),
            });
        }

        surreal_row_to_entity(&entity.schema, &rows[0])
    }

    async fn delete(&self, schema: &SchemaName, id: &EntityId) -> Result<(), BackendError> {
        let table = schema.as_str();
        let id_str = id.as_str();

        // First check if it exists
        let check_sql = format!("SELECT * FROM {table}:`{id_str}`;");
        let rows = self.execute_and_take_rows(&check_sql).await?;

        if rows.is_empty() {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id_str.to_string(),
            });
        }

        let sql = format!("DELETE {table}:`{id_str}`;");
        self.execute_raw(&sql).await?;
        Ok(())
    }

    async fn query(&self, query: &Query) -> Result<QueryResult, BackendError> {
        // Resolve schema name from the query's SchemaId.
        // The query stores a SchemaId; we need a table name.
        // For now, we use the SchemaId's string representation.
        // In a full system, we would look up the SchemaName from the SchemaId.
        let table = query.schema.as_str();

        // For SurrealDB, we use the SchemaId prefix as a table name hint.
        // But SchemaId is "schema_<uuid>", which is not a valid table name.
        // The caller should provide the table name via a helper.
        // For now, we generate the SQL and execute it.
        let sql = query_to_surql(query, table);

        let rows = self.execute_and_take_rows(&sql).await?;

        // We cannot easily determine the SchemaName from a SchemaId here.
        // For queries that go through the EntityStore, the caller knows the schema name.
        // We'll construct entities with a placeholder name parsed from the query table.
        let schema_name =
            SchemaName::new(table).unwrap_or_else(|_| SchemaName::new("Unknown").unwrap());

        let mut entities = Vec::new();
        for row in &rows {
            entities.push(surreal_row_to_entity(&schema_name, row)?);
        }

        Ok(QueryResult::new(entities, None))
    }
}

/// Convert a `surrealdb::sql::Value` response row to an `Entity`.
///
/// This is the primary deserialization path. It works directly with
/// `surrealdb::sql::Value` (the core value type) which handles `Thing`
/// record IDs natively, avoiding the serialization errors that occur
/// when trying to deserialize SurrealDB internal types through serde.
fn surreal_row_to_entity(
    schema: &SchemaName,
    row: &surrealdb::sql::Value,
) -> Result<Entity, BackendError> {
    match row {
        surrealdb::sql::Value::Object(obj) => {
            // Extract ID
            let id_value = obj.get("id").ok_or_else(|| BackendError::Internal {
                message: "SurrealDB record missing 'id' field".to_string(),
            })?;

            let id_str = extract_id_from_surreal(id_value);
            let entity_id = EntityId::parse(&id_str).map_err(|e| BackendError::Internal {
                message: format!("failed to parse entity ID '{id_str}': {e}"),
            })?;

            // Convert remaining fields
            let mut fields = BTreeMap::new();
            for (k, v) in obj.iter() {
                if k == "id" {
                    continue;
                }
                fields.insert(k.clone(), surreal_to_dynamic(v)?);
            }

            Ok(Entity::with_id(entity_id, schema.clone(), fields))
        }
        other => Err(BackendError::Internal {
            message: format!("expected Object in query result, got: {other}"),
        }),
    }
}

/// Extract entity ID string from a `surrealdb::sql::Value`.
///
/// SurrealDB returns IDs as `Thing` (table:id), `Strand` (string), or other formats.
fn extract_id_from_surreal(value: &surrealdb::sql::Value) -> String {
    match value {
        surrealdb::sql::Value::Thing(thing) => {
            // thing.id is the record's unique part
            thing.id.to_raw()
        }
        surrealdb::sql::Value::Strand(s) => s.0.clone(),
        other => other.to_string(),
    }
}

/// Convert a surrealdb::sql::Value to a SurrealQL literal string for use in SET clauses.
fn field_surreal_value_to_literal(value: &surrealdb::sql::Value) -> String {
    match value {
        surrealdb::sql::Value::None | surrealdb::sql::Value::Null => "NONE".to_string(),
        surrealdb::sql::Value::Bool(b) => b.to_string(),
        surrealdb::sql::Value::Number(n) => n.to_string(),
        surrealdb::sql::Value::Strand(s) => format!("'{}'", s.as_str().replace('\'', "\\'")),
        surrealdb::sql::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(field_surreal_value_to_literal).collect();
            format!("[{}]", items.join(", "))
        }
        surrealdb::sql::Value::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .map(|(k, v)| format!("{k}: {}", field_surreal_value_to_literal(v)))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        other => format!("'{}'", other.to_string().replace('\'', "\\'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_memory_via_url() {
        let result = SurrealBackend::connect("mem://", "test", "test").await;
        assert!(result.is_ok(), "connect(\"mem://\") should succeed");
    }

    #[tokio::test]
    async fn connect_invalid_url() {
        let result = SurrealBackend::connect("badscheme://x", "a", "b").await;
        assert!(result.is_err(), "connect with invalid scheme should fail");
        if let Err(BackendError::ConnectionError { message }) = result {
            assert!(
                message.contains("badscheme"),
                "error should mention the bad scheme"
            );
        } else {
            panic!("expected ConnectionError");
        }
    }

    #[test]
    fn extract_id_from_thing() {
        use surrealdb::sql::{Id, Thing};
        let thing = Thing::from(("Contact", Id::String("entity_abc123".into())));
        let thing_val = surrealdb::sql::Value::Thing(thing);
        assert_eq!(extract_id_from_surreal(&thing_val), "entity_abc123");
    }

    #[test]
    fn extract_id_from_strand() {
        let strand = surrealdb::sql::Strand::from("entity_abc123");
        let strand_val = surrealdb::sql::Value::Strand(strand);
        assert_eq!(extract_id_from_surreal(&strand_val), "entity_abc123");
    }
}
