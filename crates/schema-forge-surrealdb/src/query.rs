//! Pure functions for translating the schema-forge-core query IR
//! (Filter, Query) to SurrealQL SELECT strings.
//!
//! No I/O. No side effects.

use schema_forge_core::query::{AggregateOp, AggregateQuery, FieldPath, Filter, Query, SortOrder};
use schema_forge_core::types::DynamicValue;

/// Compile a `Query` to a complete SurrealQL SELECT statement.
///
/// The `table` argument is the SurrealDB table name (derived from `SchemaName`).
pub fn query_to_surql(query: &Query, table: &str) -> String {
    let mut sql = format!("SELECT * FROM {table}");

    if let Some(filter) = &query.filter {
        sql.push_str(&format!(" WHERE {}", filter_to_surql(filter)));
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
                format!("{} {dir}", field_path_to_surql(path))
            })
            .collect();
        sql.push_str(&clauses.join(", "));
    }

    if let Some(limit) = query.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }

    if let Some(offset) = query.offset {
        sql.push_str(&format!(" START {offset}"));
    }

    sql.push(';');
    sql
}

/// Compile a `Query` to a SurrealQL `SELECT count() ... GROUP ALL` statement.
///
/// Ignores limit, offset, and sort — only applies the filter.
pub fn count_to_surql(query: &Query, table: &str) -> String {
    let mut sql = format!("SELECT count() FROM {table}");

    if let Some(filter) = &query.filter {
        sql.push_str(&format!(" WHERE {}", filter_to_surql(filter)));
    }

    sql.push_str(" GROUP ALL;");
    sql
}

/// Compile a `Filter` to a SurrealQL WHERE clause fragment (no leading WHERE).
pub fn filter_to_surql(filter: &Filter) -> String {
    match filter {
        Filter::Eq { path, value } => {
            format!(
                "{} = {}",
                field_path_to_surql(path),
                dynamic_value_to_surql_literal(value)
            )
        }
        Filter::Ne { path, value } => {
            format!(
                "{} != {}",
                field_path_to_surql(path),
                dynamic_value_to_surql_literal(value)
            )
        }
        Filter::Gt { path, value } => {
            format!(
                "{} > {}",
                field_path_to_surql(path),
                dynamic_value_to_surql_literal(value)
            )
        }
        Filter::Gte { path, value } => {
            format!(
                "{} >= {}",
                field_path_to_surql(path),
                dynamic_value_to_surql_literal(value)
            )
        }
        Filter::Lt { path, value } => {
            format!(
                "{} < {}",
                field_path_to_surql(path),
                dynamic_value_to_surql_literal(value)
            )
        }
        Filter::Lte { path, value } => {
            format!(
                "{} <= {}",
                field_path_to_surql(path),
                dynamic_value_to_surql_literal(value)
            )
        }
        Filter::Contains { path, value } => {
            format!(
                "{} CONTAINS '{}'",
                field_path_to_surql(path),
                escape_surql_string(value)
            )
        }
        Filter::StartsWith { path, value } => {
            format!(
                "string::startsWith({}, '{}')",
                field_path_to_surql(path),
                escape_surql_string(value)
            )
        }
        Filter::In { path, values } => {
            let literals: Vec<String> = values.iter().map(dynamic_value_to_surql_literal).collect();
            format!("{} IN [{}]", field_path_to_surql(path), literals.join(", "))
        }
        Filter::And { filters } => {
            if filters.is_empty() {
                return "true".to_string();
            }
            let parts: Vec<String> = filters.iter().map(filter_to_surql).collect();
            format!("({})", parts.join(" AND "))
        }
        Filter::Or { filters } => {
            if filters.is_empty() {
                return "false".to_string();
            }
            let parts: Vec<String> = filters.iter().map(filter_to_surql).collect();
            format!("({})", parts.join(" OR "))
        }
        Filter::Not { filter } => {
            format!("!({})", filter_to_surql(filter))
        }
        _ => {
            // Future Filter variants -- produce a true literal so queries still run.
            "true".to_string()
        }
    }
}

/// Convert a `DynamicValue` to a SurrealQL literal string.
pub fn dynamic_value_to_surql_literal(value: &DynamicValue) -> String {
    match value {
        DynamicValue::Null => "NONE".to_string(),
        DynamicValue::Text(s) => format!("'{}'", escape_surql_string(s)),
        DynamicValue::Integer(i) => i.to_string(),
        DynamicValue::Float(f) => format!("{f}"),
        DynamicValue::Boolean(b) => b.to_string(),
        DynamicValue::DateTime(dt) => {
            format!("d'{}'", dt.to_rfc3339())
        }
        DynamicValue::Enum(s) => format!("'{}'", escape_surql_string(s)),
        DynamicValue::Json(v) => v.to_string(),
        DynamicValue::Array(arr) => {
            let items: Vec<String> = arr.iter().map(dynamic_value_to_surql_literal).collect();
            format!("[{}]", items.join(", "))
        }
        DynamicValue::Composite(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", dynamic_value_to_surql_literal(v)))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        DynamicValue::Ref(id) => {
            // EntityId is a TypeID like "entity_01h455vb4pex5vsknk084sn02q".
            // For SurrealDB record links, we store the raw ID string.
            format!("'{}'", escape_surql_string(id.as_str()))
        }
        DynamicValue::RefArray(ids) => {
            let items: Vec<String> = ids
                .iter()
                .map(|id| format!("'{}'", escape_surql_string(id.as_str())))
                .collect();
            format!("[{}]", items.join(", "))
        }
        _ => {
            // Future DynamicValue variants -- produce a debug string literal.
            format!("'{}'", escape_surql_string(&format!("{value:?}")))
        }
    }
}

/// Compile an `AggregateQuery` to a SurrealQL SELECT statement with aggregate functions.
///
/// Uses index-based aliases (`agg_0`, `agg_1`, ...) for predictable result keys.
/// Applies an optional WHERE filter and always ends with GROUP ALL.
pub fn aggregate_to_surql(query: &AggregateQuery, table: &str) -> String {
    let projections: Vec<String> = query
        .ops
        .iter()
        .enumerate()
        .map(|(i, op)| match op {
            AggregateOp::Count => format!("count() AS agg_{i}"),
            AggregateOp::Sum { field } => {
                format!("math::sum({}) AS agg_{i}", field_path_to_surql(field))
            }
            AggregateOp::Avg { field } => {
                format!("math::mean({}) AS agg_{i}", field_path_to_surql(field))
            }
            _ => format!("count() AS agg_{i}"),
        })
        .collect();

    let mut sql = format!("SELECT {} FROM {table}", projections.join(", "));

    if let Some(filter) = &query.filter {
        sql.push_str(&format!(" WHERE {}", filter_to_surql(filter)));
    }

    sql.push_str(" GROUP ALL;");
    sql
}

/// Convert a `FieldPath` to its SurrealQL dotted representation.
///
/// SurrealDB natively supports dotted paths for record link traversal,
/// so `company.industry` works directly.
pub(crate) fn field_path_to_surql(path: &FieldPath) -> String {
    path.segments().join(".")
}

/// Escape single quotes in strings for SurrealQL string literals.
fn escape_surql_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::query::FieldPath;
    use schema_forge_core::types::{DynamicValue, SchemaId};

    #[test]
    fn simple_select_all() {
        let q = Query::new(SchemaId::new());
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT * FROM Contact;");
    }

    #[test]
    fn select_with_eq_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::eq(
            FieldPath::single("name"),
            DynamicValue::Text("Jane".into()),
        ));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT * FROM Contact WHERE name = 'Jane';");
    }

    #[test]
    fn select_with_gt_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::gt(
            FieldPath::single("age"),
            DynamicValue::Integer(25),
        ));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT * FROM Contact WHERE age > 25;");
    }

    #[test]
    fn select_with_and_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::and(vec![
            Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into())),
            Filter::gt(FieldPath::single("age"), DynamicValue::Integer(25)),
        ]));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT * FROM Contact WHERE (name = 'Jane' AND age > 25);"
        );
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
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT * FROM Contact WHERE (status = 'Active' OR status = 'Pending');"
        );
    }

    #[test]
    fn select_with_not_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::negate(Filter::eq(
            FieldPath::single("deleted"),
            DynamicValue::Boolean(true),
        )));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT * FROM Contact WHERE !(deleted = true);");
    }

    #[test]
    fn select_with_contains() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::contains(FieldPath::single("email"), "example.com"));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT * FROM Contact WHERE email CONTAINS 'example.com';"
        );
    }

    #[test]
    fn select_with_starts_with() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::starts_with(FieldPath::single("name"), "J"));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT * FROM Contact WHERE string::startsWith(name, 'J');"
        );
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
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT * FROM Contact WHERE status IN ['Active', 'Pending'];"
        );
    }

    #[test]
    fn select_with_sort() {
        let q = Query::new(SchemaId::new())
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_sort(FieldPath::single("age"), SortOrder::Descending);
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT * FROM Contact ORDER BY name ASC, age DESC;");
    }

    #[test]
    fn select_with_limit_and_offset() {
        let q = Query::new(SchemaId::new()).with_limit(10).with_offset(20);
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT * FROM Contact LIMIT 10 START 20;");
    }

    #[test]
    fn select_with_dotted_path() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::eq(
            FieldPath::parse("company.industry").unwrap(),
            DynamicValue::Text("fintech".into()),
        ));
        let sql = query_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT * FROM Contact WHERE company.industry = 'fintech';"
        );
    }

    #[test]
    fn dynamic_value_null_literal() {
        assert_eq!(dynamic_value_to_surql_literal(&DynamicValue::Null), "NONE");
    }

    #[test]
    fn dynamic_value_text_literal() {
        assert_eq!(
            dynamic_value_to_surql_literal(&DynamicValue::Text("hello".into())),
            "'hello'"
        );
    }

    #[test]
    fn dynamic_value_integer_literal() {
        assert_eq!(
            dynamic_value_to_surql_literal(&DynamicValue::Integer(42)),
            "42"
        );
    }

    #[test]
    fn dynamic_value_boolean_literal() {
        assert_eq!(
            dynamic_value_to_surql_literal(&DynamicValue::Boolean(true)),
            "true"
        );
    }

    #[test]
    fn escape_single_quotes() {
        assert_eq!(
            dynamic_value_to_surql_literal(&DynamicValue::Text("it's".into())),
            "'it\\'s'"
        );
    }

    #[test]
    fn full_query_with_all_clauses() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::and(vec![
                Filter::gt(FieldPath::single("age"), DynamicValue::Integer(25)),
                Filter::eq(FieldPath::single("active"), DynamicValue::Boolean(true)),
            ]))
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_limit(10)
            .with_offset(0);
        let sql = query_to_surql(&q, "Contact");
        assert!(sql.starts_with("SELECT * FROM Contact WHERE"));
        assert!(sql.contains("age > 25"));
        assert!(sql.contains("active = true"));
        assert!(sql.contains("ORDER BY name ASC"));
        assert!(sql.contains("LIMIT 10"));
        assert!(sql.contains("START 0"));
    }

    #[test]
    fn count_all() {
        let q = Query::new(SchemaId::new());
        let sql = count_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT count() FROM Contact GROUP ALL;");
    }

    #[test]
    fn count_with_filter() {
        let q = Query::new(SchemaId::new()).with_filter(Filter::eq(
            FieldPath::single("active"),
            DynamicValue::Boolean(true),
        ));
        let sql = count_to_surql(&q, "Contact");
        assert_eq!(
            sql,
            "SELECT count() FROM Contact WHERE active = true GROUP ALL;"
        );
    }

    // -- aggregate_to_surql tests --

    #[test]
    fn aggregate_count_only() {
        let q = AggregateQuery::new(SchemaId::new()).with_op(AggregateOp::Count);
        let sql = aggregate_to_surql(&q, "Deal");
        assert_eq!(sql, "SELECT count() AS agg_0 FROM Deal GROUP ALL;");
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
        let sql = aggregate_to_surql(&q, "Deal");
        assert_eq!(
            sql,
            "SELECT count() AS agg_0, math::sum(value) AS agg_1, math::mean(value) AS agg_2 FROM Deal GROUP ALL;"
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
        let sql = aggregate_to_surql(&q, "Deal");
        assert_eq!(
            sql,
            "SELECT count() AS agg_0 FROM Deal WHERE active = true GROUP ALL;"
        );
    }

    #[test]
    fn aggregate_dotted_field_path() {
        let q = AggregateQuery::new(SchemaId::new()).with_op(AggregateOp::Sum {
            field: FieldPath::parse("line_items.amount").unwrap(),
        });
        let sql = aggregate_to_surql(&q, "Invoice");
        assert_eq!(
            sql,
            "SELECT math::sum(line_items.amount) AS agg_0 FROM Invoice GROUP ALL;"
        );
    }

    #[test]
    fn count_ignores_limit_and_sort() {
        let q = Query::new(SchemaId::new())
            .with_limit(10)
            .with_offset(20)
            .with_sort(FieldPath::single("name"), SortOrder::Ascending);
        let sql = count_to_surql(&q, "Contact");
        assert_eq!(sql, "SELECT count() FROM Contact GROUP ALL;");
    }
}
