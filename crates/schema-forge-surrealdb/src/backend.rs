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
use schema_forge_core::types::{DynamicValue, EntityId, SchemaDefinition, SchemaName};
use surrealdb::engine::any::Any;
use surrealdb::Surreal;

use crate::codegen::migration_step_to_surql;
use crate::query::query_to_surql;
use crate::value::entity_to_surreal_map;

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

    /// Execute a raw SurrealQL statement, returning the response.
    async fn execute_raw(&self, sql: &str) -> Result<surrealdb::Response, BackendError> {
        self.db
            .query(sql)
            .await
            .map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })
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

        let mut response = self.execute_raw(&sql).await?;
        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            return Err(BackendError::Internal {
                message: format!("CREATE returned no result for {table}:{id_str}"),
            });
        }

        json_row_to_entity(&entity.schema, &rows[0])
    }

    async fn get(&self, schema: &SchemaName, id: &EntityId) -> Result<Entity, BackendError> {
        let table = schema.as_str();
        let id_str = id.as_str();
        let sql = format!("SELECT * FROM {table}:`{id_str}`;");

        let mut response = self.execute_raw(&sql).await?;
        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id_str.to_string(),
            });
        }

        json_row_to_entity(schema, &rows[0])
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

        let mut response = self.execute_raw(&sql).await?;
        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id_str.to_string(),
            });
        }

        json_row_to_entity(&entity.schema, &rows[0])
    }

    async fn delete(&self, schema: &SchemaName, id: &EntityId) -> Result<(), BackendError> {
        let table = schema.as_str();
        let id_str = id.as_str();

        // First check if it exists
        let check_sql = format!("SELECT * FROM {table}:`{id_str}`;");
        let mut response = self.execute_raw(&check_sql).await?;
        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

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

        let mut response = self.execute_raw(&sql).await?;
        let rows: Vec<serde_json::Value> =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;

        // We cannot easily determine the SchemaName from a SchemaId here.
        // For queries that go through the EntityStore, the caller knows the schema name.
        // We'll construct entities with a placeholder name parsed from the query table.
        let schema_name =
            SchemaName::new(table).unwrap_or_else(|_| SchemaName::new("Unknown").unwrap());

        let mut entities = Vec::new();
        for row in &rows {
            entities.push(json_row_to_entity(&schema_name, row)?);
        }

        Ok(QueryResult::new(entities, None))
    }
}

/// Convert a SurrealDB JSON response row to an `Entity`.
fn json_row_to_entity(
    schema: &SchemaName,
    row: &serde_json::Value,
) -> Result<Entity, BackendError> {
    let obj = row.as_object().ok_or_else(|| BackendError::Internal {
        message: "expected JSON object in query result".to_string(),
    })?;

    // Extract ID
    let id_value = obj.get("id").ok_or_else(|| BackendError::Internal {
        message: "query result row missing 'id' field".to_string(),
    })?;

    let id_str = extract_id_from_json(id_value);
    let entity_id = EntityId::parse(&id_str).map_err(|e| BackendError::Internal {
        message: format!("failed to parse entity ID '{id_str}': {e}"),
    })?;

    // Convert remaining fields
    let mut fields = BTreeMap::new();
    for (k, v) in obj {
        if k == "id" {
            continue;
        }
        fields.insert(k.clone(), json_value_to_dynamic(v));
    }

    Ok(Entity::with_id(entity_id, schema.clone(), fields))
}

/// Extract entity ID from a SurrealDB JSON id field.
///
/// SurrealDB returns IDs in various formats:
/// - String: `"entity_xxx"` (when we set it explicitly)
/// - Object: `{"String": "entity_xxx"}` or `{"tb": "table", "id": {"String": "xxx"}}`
fn extract_id_from_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => {
            // May be "Table:entity_xxx" format
            if let Some((_table, id)) = s.split_once(':') {
                id.to_string()
            } else {
                s.clone()
            }
        }
        serde_json::Value::Object(map) => {
            // Try "String" key first (SurrealDB's Id::String representation)
            if let Some(serde_json::Value::String(s)) = map.get("String") {
                return s.clone();
            }
            // Try "id" key for nested Thing format
            if let Some(id_val) = map.get("id") {
                return extract_id_from_json(id_val);
            }
            value.to_string()
        }
        other => other.to_string(),
    }
}

/// Convert a JSON value from a SurrealDB response to a DynamicValue.
fn json_value_to_dynamic(value: &serde_json::Value) -> DynamicValue {
    match value {
        serde_json::Value::Null => DynamicValue::Null,
        serde_json::Value::Bool(b) => DynamicValue::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                DynamicValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                DynamicValue::Float(f)
            } else {
                DynamicValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => DynamicValue::Text(s.clone()),
        serde_json::Value::Array(arr) => {
            DynamicValue::Array(arr.iter().map(json_value_to_dynamic).collect())
        }
        serde_json::Value::Object(map) => {
            let mut btree = BTreeMap::new();
            for (k, v) in map {
                btree.insert(k.clone(), json_value_to_dynamic(v));
            }
            DynamicValue::Composite(btree)
        }
    }
}

/// Convert a surrealdb::sql::Value to a SurrealQL literal string for use in SET clauses.
fn field_surreal_value_to_literal(value: &surrealdb::sql::Value) -> String {
    match value {
        surrealdb::sql::Value::None | surrealdb::sql::Value::Null => "NONE".to_string(),
        surrealdb::sql::Value::Bool(b) => b.to_string(),
        surrealdb::sql::Value::Number(n) => n.to_string(),
        surrealdb::sql::Value::Strand(s) => format!("'{}'", s.to_string().replace('\'', "\\'")),
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

    #[test]
    fn extract_id_from_plain_string() {
        let val = serde_json::json!("entity_abc123");
        assert_eq!(extract_id_from_json(&val), "entity_abc123");
    }

    #[test]
    fn extract_id_from_table_colon_id() {
        let val = serde_json::json!("Contact:entity_abc123");
        assert_eq!(extract_id_from_json(&val), "entity_abc123");
    }

    #[test]
    fn extract_id_from_string_object() {
        let val = serde_json::json!({"String": "entity_abc123"});
        assert_eq!(extract_id_from_json(&val), "entity_abc123");
    }

    #[test]
    fn json_value_to_dynamic_primitives() {
        assert_eq!(
            json_value_to_dynamic(&serde_json::json!(null)),
            DynamicValue::Null
        );
        assert_eq!(
            json_value_to_dynamic(&serde_json::json!(true)),
            DynamicValue::Boolean(true)
        );
        assert_eq!(
            json_value_to_dynamic(&serde_json::json!(42)),
            DynamicValue::Integer(42)
        );
        assert_eq!(
            json_value_to_dynamic(&serde_json::json!("hello")),
            DynamicValue::Text("hello".into())
        );
    }
}
