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
    // Only the first rejected row evaluates the CASE projection. Passing
    // proofs retain one scan and emit no row data for diagnostics.
    let cases = proof
        .unsafe_values
        .iter()
        .enumerate()
        .map(|(index, check)| format!("WHEN {} THEN {index}", check.predicate))
        .collect::<Vec<_>>()
        .join(" ");
    let predicates = proof
        .unsafe_values
        .iter()
        .map(|check| check.predicate.as_str())
        .collect::<Vec<_>>()
        .join(" OR ");
    let unsafe_sql = format!("SELECT CASE {cases} END FROM {table} WHERE {predicates} LIMIT 1");
    let rejected: Option<i32> = sqlx::query_scalar(&unsafe_sql)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
    if let Some(index) = rejected {
        if let Some(check) = usize::try_from(index)
            .ok()
            .and_then(|index| proof.unsafe_values.get(index))
        {
            tracing::debug!(schema = %expected.name, field = %check.field,
                reason = check.reason, "Cedar storage certification fell back");
        }
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
    unsafe_values: Vec<UnsafeValueCheck>,
}

struct UnsafeValueCheck {
    field: String,
    reason: &'static str,
    predicate: String,
}

impl UnsafeValueCheck {
    fn new(field: &str, reason: &'static str, predicate: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason,
            predicate: predicate.into(),
        }
    }
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
        unsafe_values: vec![UnsafeValueCheck::new(
            "id",
            "identifier_shape",
            r#"NOT ("id" IS NOT NULL AND length("id") BETWEEN 26 AND 90
                AND translate(right("id", 26), '0123456789abcdefghjkmnpqrstvwxyz', '') = ''
                AND left(right("id", 26), 1) COLLATE "C" BETWEEN '0' AND '7'
                AND (length("id") = 26 OR (
                    length("id") >= 28 AND left(right("id", 27), 1) = '_'
                    AND translate(left("id", -27), 'abcdefghijklmnopqrstuvwxyz_', '') = ''
                    AND left("id", 1) COLLATE "C" BETWEEN 'a' AND 'z'
                    AND right(left("id", -27), 1) COLLATE "C" BETWEEN 'a' AND 'z'
                )))"#,
        )],
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
            proof.unsafe_values.push(UnsafeValueCheck::new(
                "_tenant",
                "tenant_identity",
                "(\"_tenant\" IS NOT NULL AND \"_tenant\" COLLATE \"C\" !~ '^[a-zA-Z0-9_:-]+$')",
            ));
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
            proof.unsafe_values.push(UnsafeValueCheck::new(
                &name,
                "required_null",
                format!("{quoted} IS NULL"),
            ));
        }
        if matches!(field.field_type, FieldType::Array(_)) {
            proof.unsafe_values.push(UnsafeValueCheck::new(&name, "array_shape", format!(
                "(array_ndims({quoted}) > 1 OR array_lower({quoted}, 1) <> 1 OR EXISTS (SELECT 1 FROM unnest({quoted}) AS item WHERE item IS NULL))"
            )));
        }
        // JSON is deliberately absent from Cedar, including required/hidden
        // JSON fields. Only selected rows need to satisfy serde_json decoding;
        // an unrelated row's payload must not disable exact authorized counts.
        if matches!(field.field_type, FieldType::DateTime) {
            // PostgreSQL's timestamps include infinity and exceed chrono's range.
            proof.unsafe_values.push(UnsafeValueCheck::new(&name, "date_range", format!(
                "({quoted} < TIMESTAMPTZ '0001-01-01 UTC' OR {quoted} >= TIMESTAMPTZ '10000-01-01 UTC')"
            )));
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
