//! Pure functions for translating the schema-forge-core query IR
//! (Filter, Query) to parameterized PostgreSQL SELECT strings.
//!
//! No I/O. No side effects. Returns SQL with `$1, $2, ...` bind
//! parameter placeholders and a parallel list of `DynamicValue` bind values.

use schema_forge_core::query::{AggregateOp, AggregateQuery, FieldPath, Filter, Query, SortOrder};
use schema_forge_core::types::DynamicValue;

/// The output of query compilation: a SQL string plus ordered bind values.
#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub sql: String,
    pub params: Vec<DynamicValue>,
}

/// Compile a `Query` to a parameterized PostgreSQL SELECT statement.
///
/// The `table` argument is the PostgreSQL table name (derived from `SchemaName`).
pub fn query_to_sql(query: &Query, table: &str) -> CompiledQuery {
    let mut params = Vec::new();
    let mut sql = format!("SELECT * FROM \"{table}\"");

    if let Some(filter) = &query.filter {
        let where_clause = filter_to_sql(filter, &mut params);
        sql.push_str(&format!(" WHERE {where_clause}"));
    }

    if !query.sort.is_empty() {
        sql.push_str(" ORDER BY ");
        let clauses: Vec<String> = query
            .sort
            .iter()
            .map(|(path, order)| {
                let dir = match order {
                    SortOrder::Ascending => "ASC",
                    SortOrder::Descending => "DESC",
                };
                format!("{} {dir}", field_path_to_sql(path))
            })
            .collect();
        sql.push_str(&clauses.join(", "));
    }

    if let Some(limit) = query.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }

    if let Some(offset) = query.offset {
        sql.push_str(&format!(" OFFSET {offset}"));
    }

    sql.push(';');
    CompiledQuery { sql, params }
}

/// Compile a `Query` to a PostgreSQL `SELECT COUNT(*)` statement.
///
/// Ignores limit, offset, and sort -- only applies the filter.
pub fn count_to_sql(query: &Query, table: &str) -> CompiledQuery {
    let mut params = Vec::new();
    let mut sql = format!("SELECT COUNT(*) AS \"count\" FROM \"{table}\"");

    if let Some(filter) = &query.filter {
        let where_clause = filter_to_sql(filter, &mut params);
        sql.push_str(&format!(" WHERE {where_clause}"));
    }

    sql.push(';');
    CompiledQuery { sql, params }
}

/// Compile an `AggregateQuery` to a PostgreSQL SELECT statement with aggregate functions.
///
/// Uses index-based aliases (`agg_0`, `agg_1`, ...) for predictable result keys.
pub fn aggregate_to_sql(query: &AggregateQuery, table: &str) -> CompiledQuery {
    let mut params = Vec::new();

    let projections: Vec<String> = query
        .ops
        .iter()
        .enumerate()
        .map(|(i, op)| match op {
            AggregateOp::Count => format!("COUNT(*) AS \"agg_{i}\""),
            AggregateOp::Sum { field } => {
                format!("COALESCE(SUM({}), 0) AS \"agg_{i}\"", field_path_to_sql(field))
            }
            AggregateOp::Avg { field } => {
                format!(
                    "COALESCE(AVG({}), 0) AS \"agg_{i}\"",
                    field_path_to_sql(field)
                )
            }
            _ => format!("COUNT(*) AS \"agg_{i}\""),
        })
        .collect();

    let mut sql = format!("SELECT {} FROM \"{table}\"", projections.join(", "));

    if let Some(filter) = &query.filter {
        let where_clause = filter_to_sql(filter, &mut params);
        sql.push_str(&format!(" WHERE {where_clause}"));
    }

    sql.push(';');
    CompiledQuery { sql, params }
}

/// Compile a `Filter` to a parameterized PostgreSQL WHERE clause fragment (no leading WHERE).
///
/// Each value is pushed into `params` and replaced with a `$N` placeholder.
pub fn filter_to_sql(filter: &Filter, params: &mut Vec<DynamicValue>) -> String {
    match filter {
        Filter::Eq { path, value } => {
            params.push(value.clone());
            format!("{} = ${}", field_path_to_sql(path), params.len())
        }
        Filter::Ne { path, value } => {
            params.push(value.clone());
            format!("{} != ${}", field_path_to_sql(path), params.len())
        }
        Filter::Gt { path, value } => {
            params.push(value.clone());
            format!("{} > ${}", field_path_to_sql(path), params.len())
        }
        Filter::Gte { path, value } => {
            params.push(value.clone());
            format!("{} >= ${}", field_path_to_sql(path), params.len())
        }
        Filter::Lt { path, value } => {
            params.push(value.clone());
            format!("{} < ${}", field_path_to_sql(path), params.len())
        }
        Filter::Lte { path, value } => {
            params.push(value.clone());
            format!("{} <= ${}", field_path_to_sql(path), params.len())
        }
        Filter::Contains { path, value } => {
            params.push(DynamicValue::Text(format!("%{value}%")));
            format!("{} ILIKE ${}", field_path_to_sql(path), params.len())
        }
        Filter::StartsWith { path, value } => {
            params.push(DynamicValue::Text(format!("{value}%")));
            format!("{} ILIKE ${}", field_path_to_sql(path), params.len())
        }
        Filter::In { path, values } => {
            if values.is_empty() {
                return "false".to_string();
            }
            let placeholders: Vec<String> = values
                .iter()
                .map(|v| {
                    params.push(v.clone());
                    format!("${}", params.len())
                })
                .collect();
            format!(
                "{} IN ({})",
                field_path_to_sql(path),
                placeholders.join(", ")
            )
        }
        Filter::And { filters } => {
            if filters.is_empty() {
                return "true".to_string();
            }
            let parts: Vec<String> = filters.iter().map(|f| filter_to_sql(f, params)).collect();
            format!("({})", parts.join(" AND "))
        }
        Filter::Or { filters } => {
            if filters.is_empty() {
                return "false".to_string();
            }
            let parts: Vec<String> = filters.iter().map(|f| filter_to_sql(f, params)).collect();
            format!("({})", parts.join(" OR "))
        }
        Filter::Not { filter } => {
            format!("NOT ({})", filter_to_sql(filter, params))
        }
        _ => {
            // Future Filter variants -- produce a true literal so queries still run.
            "true".to_string()
        }
    }
}

/// Convert a `FieldPath` to its PostgreSQL quoted representation.
///
/// For nested paths (e.g., `company.industry`), uses PostgreSQL JSONB
/// path syntax when appropriate. For simple fields, just quotes the name.
fn field_path_to_sql(path: &FieldPath) -> String {
    let segments = path.segments();
    if segments.len() == 1 {
        format!("\"{}\"", segments[0])
    } else {
        // For nested paths, the first segment is the JSONB column,
        // remaining segments are the JSON path.
        let col = &segments[0];
        let json_path: Vec<String> = segments[1..].iter().map(|s| format!("'{s}'")).collect();
        format!("\"{col}\"->>>{}", json_path.join("->>>"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::query::FieldPath;
    use schema_forge_core::types::SchemaId;

    #[test]
    fn simple_select_all() {
        let q = Query::new(SchemaId::new());
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(compiled.sql, "SELECT * FROM \"Contact\";");
        assert!(compiled.params.is_empty());
    }

    #[test]
    fn select_with_eq_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::eq(
            FieldPath::single("name"),
            DynamicValue::Text("Jane".into()),
        ));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(compiled.sql, "SELECT * FROM \"Contact\" WHERE \"name\" = $1;");
        assert_eq!(compiled.params, vec![DynamicValue::Text("Jane".into())]);
    }

    #[test]
    fn select_with_gt_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::gt(
            FieldPath::single("age"),
            DynamicValue::Integer(25),
        ));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(compiled.sql, "SELECT * FROM \"Contact\" WHERE \"age\" > $1;");
        assert_eq!(compiled.params, vec![DynamicValue::Integer(25)]);
    }

    #[test]
    fn select_with_and_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::and(vec![
            Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into())),
            Filter::gt(FieldPath::single("age"), DynamicValue::Integer(25)),
        ]));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" WHERE (\"name\" = $1 AND \"age\" > $2);"
        );
        assert_eq!(compiled.params.len(), 2);
    }

    #[test]
    fn select_with_or_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::or(vec![
            Filter::eq(
                FieldPath::single("status"),
                DynamicValue::Enum("Active".into()),
            ),
            Filter::eq(
                FieldPath::single("status"),
                DynamicValue::Enum("Pending".into()),
            ),
        ]));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" WHERE (\"status\" = $1 OR \"status\" = $2);"
        );
    }

    #[test]
    fn select_with_not_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::negate(Filter::eq(
            FieldPath::single("deleted"),
            DynamicValue::Boolean(true),
        )));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" WHERE NOT (\"deleted\" = $1);"
        );
    }

    #[test]
    fn select_with_contains() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::contains(FieldPath::single("email"), "example.com"));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" WHERE \"email\" ILIKE $1;"
        );
        assert_eq!(
            compiled.params,
            vec![DynamicValue::Text("%example.com%".into())]
        );
    }

    #[test]
    fn select_with_starts_with() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::starts_with(FieldPath::single("name"), "J"));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" WHERE \"name\" ILIKE $1;"
        );
        assert_eq!(compiled.params, vec![DynamicValue::Text("J%".into())]);
    }

    #[test]
    fn select_with_in_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::in_set(
            FieldPath::single("status"),
            vec![
                DynamicValue::Enum("Active".into()),
                DynamicValue::Enum("Pending".into()),
            ],
        ));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" WHERE \"status\" IN ($1, $2);"
        );
    }

    #[test]
    fn select_with_sort() {
        let q = Query::new(SchemaId::new())
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_sort(FieldPath::single("age"), SortOrder::Descending);
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" ORDER BY \"name\" ASC, \"age\" DESC;"
        );
    }

    #[test]
    fn select_with_limit_and_offset() {
        let q = Query::new(SchemaId::new()).with_limit(10).with_offset(20);
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT * FROM \"Contact\" LIMIT 10 OFFSET 20;"
        );
    }

    #[test]
    fn count_all() {
        let q = Query::new(SchemaId::new());
        let compiled = count_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT COUNT(*) AS \"count\" FROM \"Contact\";"
        );
    }

    #[test]
    fn count_with_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::eq(
            FieldPath::single("active"),
            DynamicValue::Boolean(true),
        ));
        let compiled = count_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT COUNT(*) AS \"count\" FROM \"Contact\" WHERE \"active\" = $1;"
        );
    }

    #[test]
    fn aggregate_count_only() {
        let q = AggregateQuery::new(SchemaId::new()).with_op(AggregateOp::Count);
        let compiled = aggregate_to_sql(&q, "Deal");
        assert_eq!(
            compiled.sql,
            "SELECT COUNT(*) AS \"agg_0\" FROM \"Deal\";"
        );
    }

    #[test]
    fn aggregate_multiple_ops() {
        let q = AggregateQuery::new(SchemaId::new())
            .with_op(AggregateOp::Count)
            .with_op(AggregateOp::Sum {
                field: FieldPath::single("value"),
            })
            .with_op(AggregateOp::Avg {
                field: FieldPath::single("value"),
            });
        let compiled = aggregate_to_sql(&q, "Deal");
        assert_eq!(
            compiled.sql,
            "SELECT COUNT(*) AS \"agg_0\", COALESCE(SUM(\"value\"), 0) AS \"agg_1\", COALESCE(AVG(\"value\"), 0) AS \"agg_2\" FROM \"Deal\";"
        );
    }

    #[test]
    fn aggregate_with_filter() {
        let q = AggregateQuery::new(SchemaId::new())
            .with_op(AggregateOp::Count)
            .with_filter(Filter::eq(
                FieldPath::single("active"),
                DynamicValue::Boolean(true),
            ));
        let compiled = aggregate_to_sql(&q, "Deal");
        assert_eq!(
            compiled.sql,
            "SELECT COUNT(*) AS \"agg_0\" FROM \"Deal\" WHERE \"active\" = $1;"
        );
    }

    #[test]
    fn count_ignores_limit_and_sort() {
        let q = Query::new(SchemaId::new())
            .with_limit(10)
            .with_offset(20)
            .with_sort(FieldPath::single("name"), SortOrder::Ascending);
        let compiled = count_to_sql(&q, "Contact");
        assert_eq!(
            compiled.sql,
            "SELECT COUNT(*) AS \"count\" FROM \"Contact\";"
        );
    }

    #[test]
    fn empty_in_filter_produces_false() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::in_set(FieldPath::single("status"), vec![]));
        let compiled = query_to_sql(&q, "Contact");
        assert_eq!(compiled.sql, "SELECT * FROM \"Contact\" WHERE false;");
    }
}
