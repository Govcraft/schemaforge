//! SurrealDB implementation of the `SchemaBackend` and `EntityStore` traits.
//!
//! This is the I/O boundary: all database communication happens here.
//! Pure logic lives in `codegen`, `query`, and `value` modules.

use std::collections::BTreeMap;

use crate::value::record_key_string;
use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::traits::{EntityStore, SchemaBackend};
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{DynamicValue, EntityId, FieldType, SchemaDefinition, SchemaName};
use surrealdb::engine::any::Any;
use surrealdb::types::ToSql;
use surrealdb::Surreal;

use crate::codegen::migration_step_to_surql;
use crate::query::{count_to_surql_with_schema, query_to_surql_with_schema};
use crate::value::{
    entity_to_surreal_map, first_negative_duration, first_oversized_bytes, surreal_to_dynamic,
};

fn supported_server_version(major: u64, minor: u64, stable: bool) -> bool {
    major == 3 && minor >= 3 && stable
}

/// The schema metadata table name used to store `SchemaDefinition` records.
const SCHEMA_META_TABLE: &str = "_schema_metadata";

/// Promote a generic SurrealDB query error to `BackendError::UniqueViolation`
/// when the message signals a unique-index conflict. SurrealDB's unique
/// indexes report failures with a message that includes the index name and
/// the phrase "already contains" — we recognise our own `uq_{table}_{field}`
/// naming convention and recover the field name from it.
fn reclassify_unique_violation(err: BackendError, table: &str) -> BackendError {
    let message = match &err {
        BackendError::QueryError { message } => message,
        _ => return err,
    };

    // SurrealDB error format (example):
    //   "Database index `uq_Contact_email` already contains 'alice@x.com', \
    //    with record `Contact:...`"
    if !message.contains("already contains") {
        return err;
    }
    let prefix = format!("uq_{table}_");
    let Some(start) = message.find(&prefix) else {
        return err;
    };
    let after = &message[start + prefix.len()..];
    let field_end = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after.len());
    if field_end == 0 {
        return err;
    }
    BackendError::UniqueViolation {
        schema: table.to_string(),
        field: after[..field_end].to_string(),
    }
}

/// SurrealDB backend for SchemaForge.
///
/// Wraps a connected `Surreal<Any>` client and implements both
/// `SchemaBackend` (DDL/metadata) and `EntityStore` (CRUD/query).
pub struct SurrealBackend {
    db: Surreal<Any>,
    version_checked: std::sync::OnceLock<()>,
}

impl SurrealBackend {
    /// Create a SurrealBackend from an existing connected client.
    ///
    /// Use this when the connection is managed externally (e.g., by acton-service's
    /// connection pooling). The caller is responsible for ensuring the client
    /// has the correct namespace and database selected.
    pub fn from_client(db: Surreal<Any>) -> Self {
        Self {
            db,
            version_checked: std::sync::OnceLock::new(),
        }
    }

    /// Get a reference to the underlying SurrealDB client.
    ///
    /// Useful when you need direct access to the client for queries not covered
    /// by the `SchemaBackend` or `EntityStore` traits (e.g., auth queries).
    pub fn client(&self) -> &Surreal<Any> {
        &self.db
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

        let backend = Self::from_client(db);
        backend.ensure_supported_version().await?;
        Ok(backend)
    }

    /// Connect to a remote SurrealDB instance.
    ///
    /// Supports ws://, wss://, http://, https://, and mem:// schemes.
    /// After connecting, optionally authenticates with root credentials,
    /// then selects the given namespace and database.
    pub async fn connect(url: &str, ns: &str, db_name: &str) -> Result<Self, BackendError> {
        Self::connect_with_auth(url, ns, db_name, None, None).await
    }

    /// Connect to a remote SurrealDB instance with optional authentication.
    ///
    /// If username and password are provided, signs in as root before
    /// selecting the namespace and database.
    pub async fn connect_with_auth(
        url: &str,
        ns: &str,
        db_name: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, BackendError> {
        let db = surrealdb::engine::any::connect(url).await.map_err(|e| {
            BackendError::ConnectionError {
                message: format!("failed to connect to {url}: {e}"),
            }
        })?;

        let backend = Self::from_client(db);
        backend.ensure_supported_version().await?;
        let db = backend.client();

        if let (Some(user), Some(pass)) = (username, password) {
            db.signin(surrealdb::opt::auth::Root {
                username: user.to_owned(),
                password: pass.to_owned(),
            })
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("authentication failed: {e}"),
            })?;
        }

        db.use_ns(ns)
            .use_db(db_name)
            .await
            .map_err(|e| BackendError::ConnectionError {
                message: format!("failed to select namespace/database: {e}"),
            })?;

        Ok(backend)
    }

    async fn ensure_supported_version(&self) -> Result<(), BackendError> {
        if self.version_checked.get().is_some() {
            return Ok(());
        }
        let version = self
            .db
            .version()
            .await
            .map_err(|error| BackendError::ConnectionError {
                message: format!("cannot verify SurrealDB server version: {error}"),
            })?;
        if !supported_server_version(version.major, version.minor, version.pre.is_empty()) {
            return Err(BackendError::ConnectionError { message: format!("SurrealDB {version} is unsupported; upgrade the server to stable 3.3 or newer within the 3.x series before using SchemaForge. Older embedded engines can lose concurrent conditional updates.") });
        }
        let _ = self.version_checked.set(());
        Ok(())
    }

    /// Execute a raw SurrealQL statement, returning the response.
    async fn execute_raw(&self, sql: &str) -> Result<surrealdb::IndexedResults, BackendError> {
        self.ensure_supported_version().await?;
        self.db
            .query(sql)
            .await
            .map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })
    }

    /// Execute a raw SurrealQL statement and extract the result as a list of
    /// `surrealdb::types::Value` objects.
    ///
    /// Uses `response.take::<surrealdb::types::Value>(0)` which bypasses serde
    /// deserialization (the SDK has a special `QueryResult<Value>` impl that
    /// wraps the core value directly). We then unwrap the `Array` variant
    /// manually to get individual row values.
    async fn execute_and_take_rows(
        &self,
        sql: &str,
    ) -> Result<Vec<surrealdb::types::Value>, BackendError> {
        let mut response = self.execute_raw(sql).await?;
        let value: surrealdb::types::Value =
            response.take(0).map_err(|e| BackendError::QueryError {
                message: e.to_string(),
            })?;
        let core_val = value;
        match core_val {
            surrealdb::types::Value::Array(arr) => Ok(arr.into_inner()),
            surrealdb::types::Value::None | surrealdb::types::Value::Null => Ok(Vec::new()),
            // Single object result (e.g. from CREATE)
            other => Ok(vec![other]),
        }
    }

    /// Build SET clause assignments for entity fields, resolving relation fields
    /// to proper SurrealDB record reference literals.
    async fn build_field_assignments(&self, entity: &Entity) -> Result<String, BackendError> {
        let field_map = entity_to_surreal_map(entity);

        // Look up schema to resolve relation target table names
        let schema_def = self
            .load_schema_metadata(&entity.schema)
            .await
            .ok()
            .flatten();

        let mut assignments = Vec::new();
        for (k, v) in &field_map {
            if k == "id" {
                continue;
            }

            // Fail closed on a negative duration: SurrealDB's native `duration`
            // type is unsigned, so a negative value cannot be stored faithfully.
            // Reject the write with a clear, actionable error rather than
            // silently coercing the supplied value to NULL.
            if let Some(field_value) = entity.fields.get(k.as_str()) {
                if let Some(neg) = first_negative_duration(field_value) {
                    return Err(BackendError::ValidationFailed {
                        field: k.clone(),
                        reason: format!(
                            "SurrealDB duration columns are unsigned; negative duration {} \
                             cannot be stored",
                            schema_forge_core::types::format_go_duration(&neg)
                        ),
                    });
                }
            }

            // Enforce a `bytes` field's `max_size` fail-closed on write: an
            // oversized value is rejected (HTTP 422) rather than silently stored.
            if let (Some(field_value), Some(fd)) = (
                entity.fields.get(k.as_str()),
                schema_def.as_ref().and_then(|s| s.field(k)),
            ) {
                if let Some((len, max)) = first_oversized_bytes(&fd.field_type, field_value) {
                    return Err(BackendError::ValidationFailed {
                        field: k.clone(),
                        reason: format!(
                            "bytes value of {len} bytes exceeds the field's max_size of {max} bytes"
                        ),
                    });
                }
            }

            let literal = match (
                entity.fields.get(k.as_str()),
                schema_def.as_ref().and_then(|s| s.field(k)),
            ) {
                // Ref with known target table → record reference literal
                (Some(DynamicValue::Ref(ref_id)), Some(fd)) => {
                    if let FieldType::Relation { target, .. } = &fd.field_type {
                        format!("{}:`{}`", target.as_str(), ref_id.as_str())
                    } else {
                        field_surreal_value_to_literal(v)
                    }
                }
                // RefArray with known target table → array of record references
                (Some(DynamicValue::RefArray(ref_ids)), Some(fd)) => {
                    if let FieldType::Relation { target, .. } = &fd.field_type {
                        let refs: Vec<String> = ref_ids
                            .iter()
                            .map(|id| format!("{}:`{}`", target.as_str(), id.as_str()))
                            .collect();
                        format!("[{}]", refs.join(", "))
                    } else {
                        field_surreal_value_to_literal(v)
                    }
                }
                _ => field_surreal_value_to_literal(v),
            };

            assignments.push(format!("{k} = {literal}"));
        }

        Ok(assignments.join(", "))
    }
}

impl SurrealBackend {
    async fn compile_migration(
        &self,
        schema_name: &SchemaName,
        steps: &[MigrationStep],
    ) -> Result<Vec<String>, BackendError> {
        let table = schema_name.as_str();
        let needs_enum_metadata = steps
            .iter()
            .any(|step| matches!(step, MigrationStep::RenameField { .. }))
            || steps.iter().any(|step| {
                matches!(
                    step,
                    MigrationStep::ChangeType {
                        old_type: FieldType::Enum(_),
                        new_type: FieldType::Enum(_),
                        ..
                    }
                )
            });
        let metadata = if needs_enum_metadata {
            self.load_schema_metadata(schema_name).await?
        } else {
            None
        };
        let mut statements = Vec::new();
        let mut rename_cleanup = Vec::new();
        for step in steps {
            let mut compiled = if let MigrationStep::RenameField { old_name, new_name } = step {
                let schema = metadata
                    .as_ref()
                    .ok_or_else(|| BackendError::MigrationFailed {
                        step: step.to_string(),
                        reason: "rename requires stored schema metadata".into(),
                    })?;
                let field = schema.field(old_name.as_str()).ok_or_else(|| {
                    BackendError::MigrationFailed {
                        step: step.to_string(),
                        reason: "rename source missing from stored metadata".into(),
                    }
                })?;
                // SurrealDB's transaction-local field refresh after REMOVE FIELD
                // can strip unrelated object data on a later UPDATE. Complete
                // every data write before removing renamed source definitions.
                rename_cleanup.push(format!("REMOVE FIELD {old_name} ON {table};"));
                crate::codegen::rename_field_stmts(
                    table,
                    field,
                    new_name,
                    schema.unique_scoped_by_tenant(),
                )
            } else {
                migration_step_to_surql(table, step)
            };
            if let MigrationStep::ChangeType {
                name,
                old_type: FieldType::Enum(_),
                new_type: new_type @ FieldType::Enum(_),
                ..
            } = step
            {
                let original_name = steps
                    .iter()
                    .find_map(|candidate| match candidate {
                        MigrationStep::RenameField { old_name, new_name } if new_name == name => {
                            Some(old_name)
                        }
                        _ => None,
                    })
                    .unwrap_or(name);
                let field = metadata.as_ref().and_then(|schema| schema.field(original_name.as_str())).ok_or_else(|| BackendError::MigrationFailed { step: step.to_string(), reason: "enum migration requires stored field metadata to preserve required/default modifiers".into() })?;
                let mut field = field.clone();
                field.name = name.clone();
                field.field_type = new_type.clone();
                compiled.pop();
                compiled.extend(
                    crate::codegen::define_field_stmts(table, &field)
                        .into_iter()
                        .map(|sql| sql.replacen("DEFINE FIELD ", "DEFINE FIELD OVERWRITE ", 1)),
                );
            }
            statements.extend(compiled);
        }
        statements.extend(rename_cleanup);
        Ok(statements)
    }
}

impl SchemaBackend for SurrealBackend {
    async fn apply_schema_change(
        &self,
        name: &SchemaName,
        steps: &[MigrationStep],
        definition: Option<&SchemaDefinition>,
    ) -> Result<(), BackendError> {
        self.ensure_supported_version().await?;
        if let Some(definition) = definition {
            if &definition.name != name {
                return Err(BackendError::MigrationFailed {
                    step: "atomic schema change".into(),
                    reason: "schema name does not match metadata".into(),
                });
            }
            if let Some(existing) = self.load_schema_metadata(name).await? {
                schema_forge_core::migration::DiffEngine::validate_transition(
                    &existing, definition,
                )
                .map_err(|error| BackendError::MigrationFailed {
                    step: "validate schema transition".into(),
                    reason: error.to_string(),
                })?;
            }
        }
        let mut statements = self.compile_migration(name, steps).await?;
        if let Some(definition) = definition {
            let json =
                serde_json::to_string(definition).map_err(|error| BackendError::Internal {
                    message: error.to_string(),
                })?;
            statements.push(format!("UPSERT {SCHEMA_META_TABLE}:`{name}` CONTENT {{ name: '{name}', definition: $schema_definition }};"));
            self.db
                .query(format!(
                    "BEGIN TRANSACTION;\n{}\nCOMMIT TRANSACTION;",
                    statements.join("\n")
                ))
                .bind(("schema_definition", json))
                .await
                .map_err(|error| BackendError::MigrationFailed {
                    step: "atomic schema change".into(),
                    reason: error.to_string(),
                })?
                .check()
                .map_err(|error| BackendError::MigrationFailed {
                    step: "atomic schema change".into(),
                    reason: error.to_string(),
                })?;
        } else {
            statements.push(format!("DELETE {SCHEMA_META_TABLE}:`{name}`;"));
            self.execute_raw(&format!(
                "BEGIN TRANSACTION;\n{}\nCOMMIT TRANSACTION;",
                statements.join("\n")
            ))
            .await?
            .check()
            .map_err(|error| BackendError::MigrationFailed {
                step: "atomic schema change".into(),
                reason: error.to_string(),
            })?;
        }
        Ok(())
    }

    async fn apply_migration(
        &self,
        schema_name: &SchemaName,
        steps: &[MigrationStep],
    ) -> Result<(), BackendError> {
        let statements = self.compile_migration(schema_name, steps).await?;
        if !statements.is_empty() {
            let sql = format!(
                "BEGIN TRANSACTION;\n{}\nCOMMIT TRANSACTION;",
                statements.join("\n")
            );
            self.execute_raw(&sql).await?.check().map_err(|error| {
                BackendError::MigrationFailed {
                    step: "apply migration transaction".into(),
                    reason: error.to_string(),
                }
            })?;
        }
        Ok(())
    }

    async fn store_schema_metadata(
        &self,
        definition: &SchemaDefinition,
    ) -> Result<(), BackendError> {
        if let Some(existing) = self.load_schema_metadata(&definition.name).await? {
            schema_forge_core::migration::DiffEngine::validate_transition(&existing, definition)
                .map_err(|error| BackendError::MigrationFailed {
                    step: "validate schema transition".into(),
                    reason: error.to_string(),
                })?;
        }
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

        let rows = take_metadata_rows(&mut response)?;

        if rows.is_empty() {
            return Ok(None);
        }

        let def_str = rows[0]
            .get("definition")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackendError::Internal {
                message: "schema metadata record missing 'definition' field".to_string(),
            })?;

        let definition = parse_and_sanitize_definition(name_str, def_str)?;
        Ok(Some(definition))
    }

    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        let sql = format!("SELECT definition FROM {SCHEMA_META_TABLE};");
        let mut response = self.execute_raw(&sql).await?;

        let rows = take_metadata_rows(&mut response)?;

        let mut definitions = Vec::new();
        for row in &rows {
            let def_str = row
                .get("definition")
                .and_then(|v| v.as_str())
                .ok_or_else(|| BackendError::Internal {
                    message: "schema metadata record missing 'definition' field".to_string(),
                })?;

            definitions.push(parse_and_sanitize_definition("<unknown>", def_str)?);
        }

        Ok(definitions)
    }
}

/// An absent metadata table represents a fresh database. Other missing resources
/// and malformed metadata remain errors; reads never initialize database state.
fn take_metadata_rows(
    response: &mut surrealdb::IndexedResults,
) -> Result<Vec<serde_json::Value>, BackendError> {
    match response.take(0) {
        Ok(rows) => Ok(rows),
        Err(error)
            if matches!(
                error.not_found_details(),
                Some(surrealdb::types::NotFoundError::Table { name })
                    if name == SCHEMA_META_TABLE
            ) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(BackendError::QueryError {
            message: error.to_string(),
        }),
    }
}

/// Parse a stored schema metadata JSON blob and migrate any legacy
/// `@widget("...")` annotations on the fly. Removed widget tokens (e.g.
/// `currency`, `relative_time`) are dropped from the field's annotation list
/// and `link` is rewritten to `url`, so the server can come up after a
/// breaking widget-vocabulary change without a hand-rolled DB patch.
fn parse_and_sanitize_definition(
    schema_label: &str,
    def_str: &str,
) -> Result<SchemaDefinition, BackendError> {
    let mut json: serde_json::Value =
        serde_json::from_str(def_str).map_err(|e| BackendError::Internal {
            message: format!("failed to deserialize schema metadata: {e}"),
        })?;
    let schema = json
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(schema_label)
        .to_string();
    for repair in schema_forge_core::types::sanitize_schema_metadata_json(&mut json) {
        match repair {
            schema_forge_core::types::WidgetRepair::Remapped { from, to } => {
                tracing::warn!(
                    schema = %schema,
                    from = %from,
                    to = %to,
                    "schema metadata: remapped legacy @widget token; rerun `schemaforge apply` to persist the fix"
                );
            }
            schema_forge_core::types::WidgetRepair::Dropped { token } => {
                tracing::warn!(
                    schema = %schema,
                    token = %token,
                    "schema metadata: dropped unknown @widget token; rerun `schemaforge apply` to persist the fix"
                );
            }
        }
    }
    serde_json::from_value(json).map_err(|e| BackendError::Internal {
        message: format!("failed to deserialize schema metadata: {e}"),
    })
}

impl EntityStore for SurrealBackend {
    async fn create(&self, entity: &Entity) -> Result<Entity, BackendError> {
        let table = entity.schema.as_str();
        let id_str = entity.id.as_str();

        let set_clause = self.build_field_assignments(entity).await?;
        let sql = format!("CREATE {table}:`{id_str}` SET {set_clause};");

        let rows = self
            .execute_and_take_rows(&sql)
            .await
            .map_err(|e| reclassify_unique_violation(e, table))?;

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

        let set_clause = self.build_field_assignments(entity).await?;
        let sql = format!("UPDATE {table}:`{id_str}` SET {set_clause};");

        let rows = self
            .execute_and_take_rows(&sql)
            .await
            .map_err(|e| reclassify_unique_violation(e, table))?;

        if rows.is_empty() {
            return Err(BackendError::EntityNotFound {
                schema: table.to_string(),
                entity_id: id_str.to_string(),
            });
        }

        surreal_row_to_entity(&entity.schema, &rows[0])
    }

    async fn update_field_if_matches(
        &self,
        schema: &SchemaName,
        id: &EntityId,
        field: &schema_forge_core::types::FieldName,
        expected: &DynamicValue,
        value: &DynamicValue,
    ) -> Result<bool, BackendError> {
        let literal = field_surreal_value_to_literal(&crate::value::dynamic_to_surreal(expected));
        let new_value = field_surreal_value_to_literal(&crate::value::dynamic_to_surreal(value));
        let sql = format!(
            "UPDATE `{schema}`:`{id}` SET `{field}` = {new_value} WHERE `{field}` = {literal} RETURN AFTER;"
        );
        let mut response = self.execute_raw(&sql).await?;
        match response.take::<surrealdb::types::Value>(0) {
            Ok(value) => Ok(match value {
                surrealdb::types::Value::Array(rows) => !rows.is_empty(),
                surrealdb::types::Value::None | surrealdb::types::Value::Null => false,
                _ => true,
            }),
            Err(error)
                if error.query_details()
                    == Some(&surrealdb::types::QueryError::TransactionConflict) =>
            {
                Ok(false)
            }
            Err(error) => Err(BackendError::QueryError {
                message: error.to_string(),
            }),
        }
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
        // Resolve the table name from the SchemaId by scanning schema metadata.
        let all_schemas = self.list_schema_metadata().await?;
        let schema_def = all_schemas
            .iter()
            .find(|s| s.id == query.schema)
            .ok_or_else(|| BackendError::QueryError {
                message: format!("no schema found for id '{}'", query.schema.as_str()),
            })?;

        let table = schema_def.name.as_str();
        let sql = query_to_surql_with_schema(query, table, Some(schema_def));
        let rows = self.execute_and_take_rows(&sql).await?;

        let schema_name = schema_def.name.clone();
        let mut entities = Vec::new();
        for row in &rows {
            entities.push(surreal_row_to_entity(&schema_name, row)?);
        }

        // Also compute the total matching rows (ignoring limit/offset) so
        // paginated list envelopes can report an accurate total. Skipped
        // on internal lookups that set `include_total = false`.
        let total = if query.include_total {
            Some(self.count(query).await?)
        } else {
            None
        };
        Ok(QueryResult::new(entities, total))
    }

    async fn count(&self, query: &Query) -> Result<usize, BackendError> {
        let all_schemas = self.list_schema_metadata().await?;
        let schema_def = all_schemas
            .iter()
            .find(|s| s.id == query.schema)
            .ok_or_else(|| BackendError::QueryError {
                message: format!("no schema found for id '{}'", query.schema.as_str()),
            })?;

        let table = schema_def.name.as_str();
        let sql = count_to_surql_with_schema(query, table, Some(schema_def));
        let rows = self.execute_and_take_rows(&sql).await?;

        // SurrealDB returns [{ "count": N }] for GROUP ALL, or [] if no rows.
        if let Some(surrealdb::types::Value::Object(obj)) = rows.first() {
            if let Some(surrealdb::types::Value::Number(n)) = obj.get("count") {
                return Ok(n
                    .to_int()
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(0));
            }
        }
        Ok(0)
    }

    async fn aggregate(
        &self,
        query: &AggregateQuery,
    ) -> Result<Vec<AggregateResult>, BackendError> {
        let all_schemas = self.list_schema_metadata().await?;
        let schema_def = all_schemas
            .iter()
            .find(|s| s.id == query.schema)
            .ok_or_else(|| BackendError::QueryError {
                message: format!("no schema found for id '{}'", query.schema.as_str()),
            })?;

        let table = schema_def.name.as_str();
        let sql = crate::query::aggregate_to_surql(query, table);
        let rows = self.execute_and_take_rows(&sql).await?;

        let mut results = Vec::with_capacity(query.ops.len());
        if let Some(row) = rows.first() {
            if let surrealdb::types::Value::Object(obj) = row {
                for (i, op) in query.ops.iter().enumerate() {
                    let key = format!("agg_{i}");
                    let value = match obj.get(&key) {
                        Some(surrealdb::types::Value::Number(n)) => {
                            let v = n.to_f64().unwrap_or(f64::NAN);
                            if v.is_nan() {
                                0.0
                            } else {
                                v
                            }
                        }
                        _ => 0.0,
                    };
                    results.push(AggregateResult {
                        op: op.clone(),
                        value,
                    });
                }
            }
        } else {
            // Empty table — return 0 for all ops
            for op in &query.ops {
                results.push(AggregateResult {
                    op: op.clone(),
                    value: 0.0,
                });
            }
        }

        Ok(results)
    }
}

/// Convert a `surrealdb::types::Value` response row to an `Entity`.
///
/// This is the primary deserialization path. It works directly with
/// `surrealdb::types::Value` (the core value type) which handles `Thing`
/// record IDs natively, avoiding the serialization errors that occur
/// when trying to deserialize SurrealDB internal types through serde.
fn surreal_row_to_entity(
    schema: &SchemaName,
    row: &surrealdb::types::Value,
) -> Result<Entity, BackendError> {
    match row {
        surrealdb::types::Value::Object(obj) => {
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
            message: format!("expected Object in query result, got: {other:?}"),
        }),
    }
}

/// Extract entity ID string from a `surrealdb::types::Value`.
///
/// SurrealDB returns IDs as native record identifiers or strings.
fn extract_id_from_surreal(value: &surrealdb::types::Value) -> String {
    match value {
        surrealdb::types::Value::RecordId(thing) => {
            // Preserve the raw record key without SQL quoting.
            record_key_string(&thing.key)
        }
        surrealdb::types::Value::String(s) => s.clone(),
        other => other.to_sql(),
    }
}

/// Convert a surrealdb::types::Value to a SurrealQL literal string for use in SET clauses.
fn field_surreal_value_to_literal(value: &surrealdb::types::Value) -> String {
    match value {
        surrealdb::types::Value::None | surrealdb::types::Value::Null => "NONE".to_string(),
        surrealdb::types::Value::Bool(b) => b.to_string(),
        surrealdb::types::Value::Number(n) => n.to_sql(),
        surrealdb::types::Value::String(s) => {
            // Detect ISO 8601 datetime strings and use SurrealQL d'...' literal
            if chrono::DateTime::parse_from_rfc3339(s.as_str()).is_ok() {
                format!("d'{}'", s.as_str())
            } else {
                value.to_sql()
            }
        }
        surrealdb::types::Value::Datetime(dt) => {
            format!("d'{}'", (*dt).into_inner().to_rfc3339())
        }
        // Duration literals are bare in SurrealQL (e.g. `2w3d`); the Display impl
        // produces a parseable form.
        surrealdb::types::Value::Duration(dur) => dur.to_sql(),
        // The `Bytes` Display impl emits a parseable SurrealQL literal of the form
        // `encoding::base64::decode("...")`, round-tripping to native bytes.
        surrealdb::types::Value::Bytes(b) => b.to_sql(),
        surrealdb::types::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(field_surreal_value_to_literal).collect();
            format!("[{}]", items.join(", "))
        }
        surrealdb::types::Value::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        surrealdb::types::Value::String(k.clone()).to_sql(),
                        field_surreal_value_to_literal(v)
                    )
                })
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        other => format!("'{}'", other.to_sql().replace('\'', "\\'")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_engines_without_supported_transaction_guarantees() {
        assert!(!super::supported_server_version(2, 6, true));
        assert!(!super::supported_server_version(3, 2, true));
        assert!(!super::supported_server_version(3, 3, false));
        assert!(super::supported_server_version(3, 3, true));
        assert!(super::supported_server_version(3, 4, true));
        assert!(!super::supported_server_version(4, 0, true));
    }

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
        use surrealdb::types::{RecordId, RecordIdKey};
        let thing = RecordId::new("Contact", RecordIdKey::String("entity_abc123".into()));
        let thing_val = surrealdb::types::Value::RecordId(thing);
        assert_eq!(extract_id_from_surreal(&thing_val), "entity_abc123");
    }

    #[test]
    fn extract_id_from_strand() {
        let strand = String::from("entity_abc123");
        let strand_val = surrealdb::types::Value::String(strand);
        assert_eq!(extract_id_from_surreal(&strand_val), "entity_abc123");
    }

    #[tokio::test]
    async fn create_with_negative_duration_is_rejected() {
        use std::collections::BTreeMap;

        let backend = SurrealBackend::connect_memory("test", "test")
            .await
            .expect("failed to connect to in-memory SurrealDB");

        let mut fields = BTreeMap::new();
        fields.insert(
            "retention".to_string(),
            DynamicValue::Duration(chrono::TimeDelta::seconds(-5)),
        );
        let entity = Entity::new(SchemaName::new("Record").unwrap(), fields);

        let result = backend.create(&entity).await;
        match result {
            Err(BackendError::ValidationFailed { field, reason }) => {
                assert_eq!(field, "retention");
                assert!(
                    reason.contains("unsigned") && reason.contains("-5s"),
                    "error should explain the unsigned constraint and echo the value, got: {reason}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn duration_literal_is_bare() {
        let dur = surrealdb::types::Duration::from(std::time::Duration::from_secs(3600));
        let val = surrealdb::types::Value::Duration(dur);
        // Bare SurrealQL duration literal, no quotes.
        assert_eq!(field_surreal_value_to_literal(&val), "1h");
    }

    #[test]
    fn bytes_literal_is_base64_decode_call() {
        let val = surrealdb::types::Value::Bytes(surrealdb::types::Bytes::from(b"hello".to_vec()));
        // Parseable SurrealQL: decodes back to the same bytes.
        assert_eq!(
            field_surreal_value_to_literal(&val),
            "encoding::base64::decode(\"aGVsbG8\")"
        );
    }

    #[tokio::test]
    async fn create_with_oversized_bytes_is_rejected() {
        use std::collections::BTreeMap;

        let backend = SurrealBackend::connect_memory("test", "test")
            .await
            .expect("failed to connect to in-memory SurrealDB");

        // Register the schema so the write path knows the field's max_size.
        let schema = SchemaDefinition::new(
            schema_forge_core::types::SchemaId::new(),
            SchemaName::new("Record").unwrap(),
            vec![schema_forge_core::types::FieldDefinition::new(
                schema_forge_core::types::FieldName::new("sig").unwrap(),
                FieldType::Bytes(schema_forge_core::types::BytesConstraints::with_max_size(4)),
            )],
            vec![],
        )
        .unwrap();
        backend
            .store_schema_metadata(&schema)
            .await
            .expect("store schema metadata");

        let mut fields = BTreeMap::new();
        fields.insert(
            "sig".to_string(),
            DynamicValue::Bytes(vec![1, 2, 3, 4, 5, 6]),
        );
        let entity = Entity::new(SchemaName::new("Record").unwrap(), fields);

        match backend.create(&entity).await {
            Err(BackendError::ValidationFailed { field, reason }) => {
                assert_eq!(field, "sig");
                assert!(
                    reason.contains("exceeds") && reason.contains("max_size"),
                    "error should explain the size cap, got: {reason}"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn reclassify_recognises_unique_index_conflict() {
        let err = BackendError::QueryError {
            message: "Database index `uq_Contact_email` already contains 'alice@x.com', with record `Contact:abc`".into(),
        };
        let mapped = reclassify_unique_violation(err, "Contact");
        match mapped {
            BackendError::UniqueViolation { schema, field } => {
                assert_eq!(schema, "Contact");
                assert_eq!(field, "email");
            }
            other => panic!("expected UniqueViolation, got {other:?}"),
        }
    }

    #[test]
    fn reclassify_leaves_unrelated_query_errors_alone() {
        let err = BackendError::QueryError {
            message: "syntax error near SELECT".into(),
        };
        match reclassify_unique_violation(err, "Contact") {
            BackendError::QueryError { .. } => {}
            other => panic!("expected QueryError, got {other:?}"),
        }
    }

    #[test]
    fn reclassify_leaves_other_table_constraint_alone() {
        let err = BackendError::QueryError {
            message: "Database index `uq_Other_email` already contains 'x'".into(),
        };
        match reclassify_unique_violation(err, "Contact") {
            BackendError::QueryError { .. } => {}
            other => panic!("expected QueryError, got {other:?}"),
        }
    }
}
