//! Conservative storage proof for row-independent Cedar reads.

use schema_forge_backend::auth::CedarReadScope;
use schema_forge_backend::entity::QueryResult;
use schema_forge_backend::error::BackendError;
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{Cardinality, DynamicValue, FieldType, SchemaDefinition};
use sqlx::postgres::{PgArguments, PgPool, PgRow};
use sqlx::Row;

use crate::query::{count_to_sql, query_to_sql};
use crate::value::{bind_dynamic_value, row_to_entity};

fn database_error(error: sqlx::Error) -> BackendError {
    BackendError::QueryError {
        message: format!("Cedar-compatible read failed: {error}"),
    }
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn arguments(values: &[DynamicValue]) -> Result<PgArguments, BackendError> {
    let mut args = PgArguments::default();
    for value in values {
        bind_dynamic_value(&mut args, value, None)?;
    }
    Ok(args)
}

/// Certify physical shape and query under one MVCC snapshot and DDL lock.
pub(crate) async fn query_compatible(
    pool: &PgPool,
    expected: &SchemaDefinition,
    query: &Query,
    scope: &CedarReadScope,
) -> Result<Option<QueryResult>, BackendError> {
    if query.projection.is_some()
        || query.schema != expected.id
        || expected.field("_tenant").is_some()
        || expected.field("id").is_some()
    {
        return Ok(None);
    }
    let table = quoted(expected.name.as_str());
    let mut tx = pool.begin().await.map_err(database_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    // Held until the page and total are read. Never reuse a proof across DDL.
    sqlx::query(&format!("LOCK TABLE {table} IN ACCESS SHARE MODE"))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT definition FROM \"_schema_metadata\" WHERE name = $1")
            .bind(expected.name.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    if serde_json::from_value::<SchemaDefinition>(stored)
        .ok()
        .as_ref()
        != Some(expected)
    {
        return Ok(None);
    }
    let columns = sqlx::query(
        "SELECT a.attname, t.typname, c.relkind::text AS kind, t.typnamespace = 'pg_catalog'::regnamespace AS builtin, COALESCE(coll.collisdeterministic, true) AS deterministic \
         FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid \
         JOIN pg_class c ON c.oid = a.attrelid \
         LEFT JOIN pg_collation coll ON coll.oid = a.attcollation \
         WHERE a.attrelid = to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped",
    )
    .bind(&table)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let Some(proof) = certify_columns(expected, &columns)? else {
        return Ok(None);
    };
    let unsafe_sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE {})",
        proof.unsafe_values.join(" OR ")
    );
    let unsafe_values: bool = sqlx::query_scalar(&unsafe_sql)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
    if unsafe_values {
        return Ok(None);
    }
    let scoped = scoped_query(query, scope, proof.has_tenant);
    let total = if scoped.include_total {
        let count = count_to_sql(&scoped, expected.name.as_str());
        let total: i64 = sqlx::query_scalar_with(&count.sql, arguments(&count.params)?)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        Some(
            usize::try_from(total).map_err(|error| BackendError::Internal {
                message: format!("Cedar-compatible count is out of range: {error}"),
            })?,
        )
    } else {
        None
    };
    let compiled = query_to_sql(&scoped, expected.name.as_str());
    let rows = sqlx::query_with(&compiled.sql, arguments(&compiled.params)?)
        .persistent(false)
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
    let entities = rows
        .iter()
        .map(|row| row_to_entity(row, &expected.name, Some(expected)))
        .collect::<Result<Vec<_>, _>>()?;
    tx.commit().await.map_err(database_error)?;
    Ok(Some(QueryResult::new(entities, total)))
}

struct StorageProof {
    has_tenant: bool,
    unsafe_values: Vec<String>,
}

fn certify_columns(
    schema: &SchemaDefinition,
    columns: &[PgRow],
) -> Result<Option<StorageProof>, BackendError> {
    let mut names = std::collections::BTreeSet::new();
    let mut proof = StorageProof {
        has_tenant: false,
        // A conservative subset of valid TypeIDs, including every generated ID.
        // Invalid IDs could prevent row decoding before Cedar is reached.
        unsafe_values: vec![r#"NOT ("id" IS NOT NULL AND length("id") BETWEEN 26 AND 90
                AND translate(right("id", 26), '0123456789abcdefghjkmnpqrstvwxyz', '') = ''
                AND left(right("id", 26), 1) COLLATE "C" BETWEEN '0' AND '7'
                AND (length("id") = 26 OR (
                    length("id") >= 28 AND left(right("id", 27), 1) = '_'
                    AND translate(left("id", -27), 'abcdefghijklmnopqrstuvwxyz_', '') = ''
                    AND left("id", 1) COLLATE "C" BETWEEN 'a' AND 'z'
                    AND right(left("id", -27), 1) COLLATE "C" BETWEEN 'a' AND 'z'
                )))"#
            .into()],
    };
    for column in columns {
        let name: String = column.try_get("attname").map_err(database_error)?;
        let sql_type: String = column.try_get("typname").map_err(database_error)?;
        let kind: String = column.try_get("kind").map_err(database_error)?;
        if kind != "r"
            || !column
                .try_get::<bool, _>("builtin")
                .map_err(database_error)?
            || !column
                .try_get::<bool, _>("deterministic")
                .map_err(database_error)?
        {
            return Ok(None);
        }
        names.insert(name.clone());
        if name == "id" {
            if sql_type != "text" && sql_type != "varchar" {
                return Ok(None);
            }
            continue;
        }
        if name == "_tenant" && schema.field(&name).is_none() {
            if sql_type != "text" && sql_type != "varchar" {
                return Ok(None);
            }
            proof.has_tenant = true;
            // The adapter constructs an EntityUid through Cedar text parsing.
            // Restrict to strings whose parsed identity equals the stored text.
            proof.unsafe_values.push(
                "(\"_tenant\" IS NOT NULL AND \"_tenant\" COLLATE \"C\" !~ '^[a-zA-Z0-9_:-]+$')"
                    .into(),
            );
            continue;
        }
        let Some(field) = schema.field(&name) else {
            return Ok(None);
        };
        let Some(expected_type) = supported_type(&field.field_type) else {
            return Ok(None);
        };
        if sql_type != expected_type && !(expected_type == "text" && sql_type == "varchar") {
            return Ok(None);
        }
        let quoted = quoted(&name);
        if !field.is_hidden() && field.is_required() && !matches!(field.field_type, FieldType::Json)
        {
            proof.unsafe_values.push(format!("{quoted} IS NULL"));
        }
        if matches!(field.field_type, FieldType::Array(_)) {
            proof.unsafe_values.push(format!(
                "(array_ndims({quoted}) > 1 OR array_lower({quoted}, 1) <> 1 OR EXISTS (SELECT 1 FROM unnest({quoted}) AS item WHERE item IS NULL))"
            ));
        }
        if matches!(field.field_type, FieldType::Json) {
            // PostgreSQL accepts JSON numbers and nesting beyond serde_json's
            // decoder limits. Its jsonb text rendering expands numeric exponents.
            // These deliberately conservative bounds also count digits/brackets
            // inside strings: uncertain shapes fall back instead of being counted.
            // CASE guarantees that short ordinary JSON never pays for the
            // expensive long-number regex or delimiter count. AND alone permits
            // the planner to reorder evaluation and loses this bound.
            proof.unsafe_values.push(format!(
                "(CASE WHEN octet_length({quoted}::text) < 100 THEN false ELSE \
                 (CASE WHEN octet_length({quoted}::text) < 300 THEN false ELSE \
                  {quoted}::text COLLATE \"C\" ~ '[0-9]{{150}}[0-9]{{150}}' END) OR \
                 length({quoted}::text) - length(translate({quoted}::text, '{{[', '')) >= 100 END)"
            ));
        }
        if matches!(field.field_type, FieldType::DateTime) {
            // PostgreSQL's timestamps include infinity and exceed chrono's range.
            proof.unsafe_values.push(format!(
                "({quoted} < TIMESTAMPTZ '0001-01-01 UTC' OR {quoted} >= TIMESTAMPTZ '10000-01-01 UTC')"
            ));
        }
    }
    if !names.contains("id")
        || schema
            .fields
            .iter()
            .any(|field| !names.contains(field.name.as_str()))
    {
        return Ok(None);
    }
    Ok(Some(proof))
}

fn supported_type(field: &FieldType) -> Option<&'static str> {
    match field {
        FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_) => Some("text"),
        FieldType::Integer(_) => Some("int8"),
        FieldType::Float(_) => Some("float8"),
        FieldType::Boolean => Some("bool"),
        FieldType::DateTime => Some("timestamptz"),
        FieldType::Json => Some("jsonb"),
        FieldType::Relation {
            cardinality: Cardinality::One,
            ..
        } => Some("text"),
        FieldType::Array(inner)
            if matches!(
                inner.as_ref(),
                FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_)
            ) =>
        {
            Some("_text")
        }
        _ => None,
    }
}

fn scoped_query(query: &Query, scope: &CedarReadScope, has_tenant: bool) -> Query {
    let mut query = query.clone();
    if let CedarReadScope::TenantMembers(members) = scope {
        if has_tenant {
            let tenant = Filter::Or {
                filters: vec![
                    Filter::eq(FieldPath::single("_tenant"), DynamicValue::Null),
                    Filter::in_set(
                        FieldPath::single("_tenant"),
                        members.iter().cloned().map(DynamicValue::Text).collect(),
                    ),
                ],
            };
            query.filter = Some(match query.filter.take() {
                Some(filter) => Filter::And {
                    filters: vec![filter, tenant],
                },
                None => tenant,
            });
        }
    }
    query
}
