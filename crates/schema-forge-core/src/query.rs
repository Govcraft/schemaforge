use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::{DynamicValue, FieldType, SchemaDefinition, SchemaId};

// ---------------------------------------------------------------------------
// FieldPath
// ---------------------------------------------------------------------------

/// A dotted path for field access, supporting relation traversal.
///
/// `"company.industry"` becomes `FieldPath(vec!["company", "industry"])`.
/// `"name"` becomes `FieldPath(vec!["name"])`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    /// Creates a new `FieldPath` from a dotted string like `"company.industry"`.
    pub fn parse(s: &str) -> Result<Self, QueryError> {
        if s.is_empty() {
            return Err(QueryError::EmptyFieldPath);
        }
        let segments: Vec<String> = s.split('.').map(String::from).collect();
        for segment in &segments {
            if segment.is_empty() {
                return Err(QueryError::InvalidFieldPath {
                    path: s.to_string(),
                    reason: "path contains empty segment".to_string(),
                });
            }
        }
        Ok(Self(segments))
    }

    /// Creates a `FieldPath` from a single field name (no dots).
    pub fn single(name: impl Into<String>) -> Self {
        Self(vec![name.into()])
    }

    /// Creates a `FieldPath` from pre-validated segments.
    pub fn from_segments(segments: Vec<String>) -> Result<Self, QueryError> {
        if segments.is_empty() {
            return Err(QueryError::EmptyFieldPath);
        }
        for segment in &segments {
            if segment.is_empty() {
                return Err(QueryError::InvalidFieldPath {
                    path: segments.join("."),
                    reason: "path contains empty segment".to_string(),
                });
            }
        }
        Ok(Self(segments))
    }

    /// Returns the path segments.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Returns the number of segments in the path.
    pub fn depth(&self) -> usize {
        self.0.len()
    }

    /// Returns true if this is a simple (single-segment) path.
    pub fn is_simple(&self) -> bool {
        self.0.len() == 1
    }

    /// Returns the first segment (the root field name).
    pub fn root(&self) -> &str {
        &self.0[0]
    }

    /// Returns the last segment (the leaf field name).
    pub fn leaf(&self) -> &str {
        &self.0[self.0.len() - 1]
    }

    /// Returns the dotted string representation.
    pub fn as_dotted(&self) -> String {
        self.0.join(".")
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join("."))
    }
}

// ---------------------------------------------------------------------------
// SortOrder
// ---------------------------------------------------------------------------

/// Sort direction for query results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl fmt::Display for SortOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ascending => write!(f, "ASC"),
            Self::Descending => write!(f, "DESC"),
        }
    }
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

/// Storage-agnostic query filter representation.
///
/// Backends compile this to their native query language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
#[non_exhaustive]
pub enum Filter {
    /// Field equals value.
    Eq {
        path: FieldPath,
        value: DynamicValue,
    },
    /// Field does not equal value.
    Ne {
        path: FieldPath,
        value: DynamicValue,
    },
    /// Field is greater than value.
    Gt {
        path: FieldPath,
        value: DynamicValue,
    },
    /// Field is greater than or equal to value.
    Gte {
        path: FieldPath,
        value: DynamicValue,
    },
    /// Field is less than value.
    Lt {
        path: FieldPath,
        value: DynamicValue,
    },
    /// Field is less than or equal to value.
    Lte {
        path: FieldPath,
        value: DynamicValue,
    },
    /// Field contains the given substring.
    Contains { path: FieldPath, value: String },
    /// Field starts with the given prefix.
    StartsWith { path: FieldPath, value: String },
    /// Field value is one of the given values.
    In {
        path: FieldPath,
        values: Vec<DynamicValue>,
    },
    /// All sub-filters must match (logical AND).
    And { filters: Vec<Filter> },
    /// At least one sub-filter must match (logical OR).
    Or { filters: Vec<Filter> },
    /// The sub-filter must NOT match (logical NOT).
    Not { filter: Box<Filter> },
}

impl Filter {
    /// Create an equality filter.
    pub fn eq(path: FieldPath, value: DynamicValue) -> Self {
        Self::Eq { path, value }
    }

    /// Create a not-equal filter.
    pub fn ne(path: FieldPath, value: DynamicValue) -> Self {
        Self::Ne { path, value }
    }

    /// Create a greater-than filter.
    pub fn gt(path: FieldPath, value: DynamicValue) -> Self {
        Self::Gt { path, value }
    }

    /// Create a greater-than-or-equal filter.
    pub fn gte(path: FieldPath, value: DynamicValue) -> Self {
        Self::Gte { path, value }
    }

    /// Create a less-than filter.
    pub fn lt(path: FieldPath, value: DynamicValue) -> Self {
        Self::Lt { path, value }
    }

    /// Create a less-than-or-equal filter.
    pub fn lte(path: FieldPath, value: DynamicValue) -> Self {
        Self::Lte { path, value }
    }

    /// Create a contains filter.
    pub fn contains(path: FieldPath, value: impl Into<String>) -> Self {
        Self::Contains {
            path,
            value: value.into(),
        }
    }

    /// Create a starts-with filter.
    pub fn starts_with(path: FieldPath, value: impl Into<String>) -> Self {
        Self::StartsWith {
            path,
            value: value.into(),
        }
    }

    /// Create an in-set filter.
    pub fn in_set(path: FieldPath, values: Vec<DynamicValue>) -> Self {
        Self::In { path, values }
    }

    /// Combine filters with AND.
    pub fn and(filters: Vec<Filter>) -> Self {
        Self::And { filters }
    }

    /// Combine filters with OR.
    pub fn or(filters: Vec<Filter>) -> Self {
        Self::Or { filters }
    }

    /// Negate a filter.
    pub fn negate(filter: Filter) -> Self {
        Self::Not {
            filter: Box::new(filter),
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Eq { path, value } => write!(f, "{path} = {value}"),
            Self::Ne { path, value } => write!(f, "{path} != {value}"),
            Self::Gt { path, value } => write!(f, "{path} > {value}"),
            Self::Gte { path, value } => write!(f, "{path} >= {value}"),
            Self::Lt { path, value } => write!(f, "{path} < {value}"),
            Self::Lte { path, value } => write!(f, "{path} <= {value}"),
            Self::Contains { path, value } => write!(f, "{path} CONTAINS \"{value}\""),
            Self::StartsWith { path, value } => write!(f, "{path} STARTS WITH \"{value}\""),
            Self::In { path, values } => {
                write!(f, "{path} IN [")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::And { filters } => {
                write!(f, "(")?;
                for (i, filter) in filters.iter().enumerate() {
                    if i > 0 {
                        write!(f, " AND ")?;
                    }
                    write!(f, "{filter}")?;
                }
                write!(f, ")")
            }
            Self::Or { filters } => {
                write!(f, "(")?;
                for (i, filter) in filters.iter().enumerate() {
                    if i > 0 {
                        write!(f, " OR ")?;
                    }
                    write!(f, "{filter}")?;
                }
                write!(f, ")")
            }
            Self::Not { filter } => write!(f, "NOT ({filter})"),
        }
    }
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// A complete, storage-agnostic query against a schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// The schema to query.
    pub schema: SchemaId,
    /// Optional filter to apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    /// Sort ordering: list of (field_path, direction) pairs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<(FieldPath, SortOrder)>,
    /// Maximum number of results to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Number of results to skip (for pagination).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

impl Query {
    /// Create a new query for a given schema with no filter, sort, or pagination.
    pub fn new(schema: SchemaId) -> Self {
        Self {
            schema,
            filter: None,
            sort: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// Set the filter.
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Add a sort clause.
    pub fn with_sort(mut self, path: FieldPath, order: SortOrder) -> Self {
        self.sort.push((path, order));
        self
    }

    /// Set the limit.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set the offset.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Validate the query structure.
    pub fn validate(&self) -> Result<(), QueryError> {
        if let (Some(limit), Some(offset)) = (self.limit, self.offset) {
            if limit == 0 {
                return Err(QueryError::InvalidLimit { limit: 0 });
            }
            // offset of 0 is valid (no skip)
            let _ = offset;
        } else if let Some(limit) = self.limit {
            if limit == 0 {
                return Err(QueryError::InvalidLimit { limit: 0 });
            }
        }
        Ok(())
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SELECT * FROM {}", self.schema)?;
        if let Some(filter) = &self.filter {
            write!(f, " WHERE {filter}")?;
        }
        if !self.sort.is_empty() {
            write!(f, " ORDER BY ")?;
            for (i, (path, order)) in self.sort.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{path} {order}")?;
            }
        }
        if let Some(limit) = self.limit {
            write!(f, " LIMIT {limit}")?;
        }
        if let Some(offset) = self.offset {
            write!(f, " START {offset}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// QueryError
// ---------------------------------------------------------------------------

/// Errors that occur during query construction or validation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryError {
    /// The field path was empty.
    EmptyFieldPath,
    /// The field path is invalid.
    InvalidFieldPath { path: String, reason: String },
    /// The limit value is invalid (must be > 0).
    InvalidLimit { limit: usize },
    /// The filter references a field that does not exist in the schema.
    UnknownField { field: String, schema: String },
    /// The filter value type does not match the field type.
    TypeMismatch {
        field: String,
        expected: String,
        actual: String,
    },
    /// The In filter has an empty values list.
    EmptyInValues { field: String },
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFieldPath => write!(f, "field path must not be empty"),
            Self::InvalidFieldPath { path, reason } => {
                write!(f, "invalid field path '{path}': {reason}")
            }
            Self::InvalidLimit { limit } => {
                write!(f, "invalid limit {limit}: must be greater than 0")
            }
            Self::UnknownField { field, schema } => {
                write!(f, "unknown field '{field}' in schema '{schema}'")
            }
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "type mismatch for field '{field}': expected {expected}, got {actual}"
                )
            }
            Self::EmptyInValues { field } => {
                write!(f, "IN filter for field '{field}' has no values")
            }
        }
    }
}

impl std::error::Error for QueryError {}

// ---------------------------------------------------------------------------
// Filter validation
// ---------------------------------------------------------------------------

/// Validate that a filter only references fields that exist in the schema.
///
/// Recursively walks the `Filter` tree. For each leaf filter, checks that the
/// root segment of the `FieldPath` exists in `schema.fields`. Returns all
/// errors found rather than stopping at the first.
pub fn validate_filter(filter: &Filter, schema: &SchemaDefinition) -> Result<(), Vec<QueryError>> {
    let mut errors = Vec::new();
    collect_filter_errors(filter, schema, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_filter_errors(
    filter: &Filter,
    schema: &SchemaDefinition,
    errors: &mut Vec<QueryError>,
) {
    match filter {
        Filter::Eq { path, value }
        | Filter::Ne { path, value }
        | Filter::Gt { path, value }
        | Filter::Gte { path, value }
        | Filter::Lt { path, value }
        | Filter::Lte { path, value } => {
            check_field_exists(path, schema, errors);
            if let Some(field_def) = schema.field(path.root()) {
                if path.is_simple() {
                    check_type_compat(path.root(), &field_def.field_type, value, errors);
                }
            }
        }
        Filter::Contains { path, .. } | Filter::StartsWith { path, .. } => {
            check_field_exists(path, schema, errors);
            if let Some(field_def) = schema.field(path.root()) {
                if path.is_simple() && !is_text_like(&field_def.field_type) {
                    errors.push(QueryError::TypeMismatch {
                        field: path.root().to_string(),
                        expected: "Text".to_string(),
                        actual: field_type_name(&field_def.field_type),
                    });
                }
            }
        }
        Filter::In { path, values } => {
            check_field_exists(path, schema, errors);
            if values.is_empty() {
                errors.push(QueryError::EmptyInValues {
                    field: path.as_dotted(),
                });
            }
            if let Some(field_def) = schema.field(path.root()) {
                if path.is_simple() {
                    for v in values {
                        check_type_compat(path.root(), &field_def.field_type, v, errors);
                    }
                }
            }
        }
        Filter::And { filters } | Filter::Or { filters } => {
            for f in filters {
                collect_filter_errors(f, schema, errors);
            }
        }
        Filter::Not { filter } => {
            collect_filter_errors(filter, schema, errors);
        }
    }
}

fn check_field_exists(path: &FieldPath, schema: &SchemaDefinition, errors: &mut Vec<QueryError>) {
    if schema.field(path.root()).is_none() {
        errors.push(QueryError::UnknownField {
            field: path.root().to_string(),
            schema: schema.name.as_str().to_string(),
        });
    }
}

fn is_text_like(ft: &FieldType) -> bool {
    matches!(ft, FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_))
}

fn field_type_name(ft: &FieldType) -> String {
    match ft {
        FieldType::Text(_) => "Text",
        FieldType::RichText => "RichText",
        FieldType::Integer(_) => "Integer",
        FieldType::Float(_) => "Float",
        FieldType::Boolean => "Boolean",
        FieldType::DateTime => "DateTime",
        FieldType::Enum(_) => "Enum",
        FieldType::Json => "Json",
        FieldType::Relation { .. } => "Relation",
        FieldType::Array(_) => "Array",
        FieldType::Composite(_) => "Composite",
    }
    .to_string()
}

fn check_type_compat(
    field_name: &str,
    field_type: &FieldType,
    value: &DynamicValue,
    errors: &mut Vec<QueryError>,
) {
    if matches!(value, DynamicValue::Null) {
        return; // null is always compatible
    }
    let compatible = match field_type {
        FieldType::Text(_) | FieldType::RichText => matches!(value, DynamicValue::Text(_)),
        FieldType::Integer(_) => matches!(value, DynamicValue::Integer(_)),
        FieldType::Float(_) => matches!(value, DynamicValue::Float(_) | DynamicValue::Integer(_)),
        FieldType::Boolean => matches!(value, DynamicValue::Boolean(_)),
        FieldType::DateTime => matches!(value, DynamicValue::DateTime(_)),
        FieldType::Enum(_) => matches!(value, DynamicValue::Enum(_) | DynamicValue::Text(_)),
        _ => true, // Json, Relation, Array, Composite — accept anything
    };
    if !compatible {
        errors.push(QueryError::TypeMismatch {
            field: field_name.to_string(),
            expected: field_type_name(field_type),
            actual: dynamic_value_type_name(value),
        });
    }
}

fn dynamic_value_type_name(value: &DynamicValue) -> String {
    match value {
        DynamicValue::Null => "Null",
        DynamicValue::Text(_) => "Text",
        DynamicValue::Integer(_) => "Integer",
        DynamicValue::Float(_) => "Float",
        DynamicValue::Boolean(_) => "Boolean",
        DynamicValue::DateTime(_) => "DateTime",
        DynamicValue::Enum(_) => "Enum",
        DynamicValue::Json(_) => "Json",
        DynamicValue::Array(_) => "Array",
        DynamicValue::Composite(_) => "Composite",
        DynamicValue::Ref(_) => "Ref",
        DynamicValue::RefArray(_) => "RefArray",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SchemaId;

    // -- FieldPath tests --

    #[test]
    fn field_path_parse_simple() {
        let fp = FieldPath::parse("name").unwrap();
        assert_eq!(fp.segments(), &["name"]);
        assert!(fp.is_simple());
        assert_eq!(fp.depth(), 1);
        assert_eq!(fp.root(), "name");
        assert_eq!(fp.leaf(), "name");
    }

    #[test]
    fn field_path_parse_dotted() {
        let fp = FieldPath::parse("company.industry").unwrap();
        assert_eq!(fp.segments(), &["company", "industry"]);
        assert!(!fp.is_simple());
        assert_eq!(fp.depth(), 2);
        assert_eq!(fp.root(), "company");
        assert_eq!(fp.leaf(), "industry");
    }

    #[test]
    fn field_path_parse_deep() {
        let fp = FieldPath::parse("a.b.c.d").unwrap();
        assert_eq!(fp.depth(), 4);
        assert_eq!(fp.root(), "a");
        assert_eq!(fp.leaf(), "d");
    }

    #[test]
    fn field_path_parse_empty_fails() {
        assert!(matches!(
            FieldPath::parse(""),
            Err(QueryError::EmptyFieldPath)
        ));
    }

    #[test]
    fn field_path_parse_double_dot_fails() {
        assert!(matches!(
            FieldPath::parse("a..b"),
            Err(QueryError::InvalidFieldPath { .. })
        ));
    }

    #[test]
    fn field_path_parse_trailing_dot_fails() {
        assert!(matches!(
            FieldPath::parse("a."),
            Err(QueryError::InvalidFieldPath { .. })
        ));
    }

    #[test]
    fn field_path_parse_leading_dot_fails() {
        assert!(matches!(
            FieldPath::parse(".a"),
            Err(QueryError::InvalidFieldPath { .. })
        ));
    }

    #[test]
    fn field_path_display() {
        let fp = FieldPath::parse("company.industry").unwrap();
        assert_eq!(fp.to_string(), "company.industry");
        assert_eq!(fp.as_dotted(), "company.industry");
    }

    #[test]
    fn field_path_single() {
        let fp = FieldPath::single("email");
        assert!(fp.is_simple());
        assert_eq!(fp.root(), "email");
    }

    #[test]
    fn field_path_from_segments() {
        let fp = FieldPath::from_segments(vec!["a".into(), "b".into()]).unwrap();
        assert_eq!(fp.depth(), 2);
    }

    #[test]
    fn field_path_from_empty_segments_fails() {
        assert!(FieldPath::from_segments(vec![]).is_err());
    }

    #[test]
    fn field_path_serde_roundtrip() {
        let fp = FieldPath::parse("company.industry").unwrap();
        let json = serde_json::to_string(&fp).unwrap();
        let back: FieldPath = serde_json::from_str(&json).unwrap();
        assert_eq!(fp, back);
    }

    // -- SortOrder tests --

    #[test]
    fn sort_order_display() {
        assert_eq!(SortOrder::Ascending.to_string(), "ASC");
        assert_eq!(SortOrder::Descending.to_string(), "DESC");
    }

    #[test]
    fn sort_order_serde_roundtrip() {
        for order in [SortOrder::Ascending, SortOrder::Descending] {
            let json = serde_json::to_string(&order).unwrap();
            let back: SortOrder = serde_json::from_str(&json).unwrap();
            assert_eq!(order, back);
        }
    }

    // -- Filter tests --

    #[test]
    fn filter_eq_display() {
        let f = Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into()));
        assert_eq!(f.to_string(), "name = \"Jane\"");
    }

    #[test]
    fn filter_ne_display() {
        let f = Filter::ne(
            FieldPath::single("status"),
            DynamicValue::Enum("Inactive".into()),
        );
        assert_eq!(f.to_string(), "status != Inactive");
    }

    #[test]
    fn filter_gt_display() {
        let f = Filter::gt(FieldPath::single("age"), DynamicValue::Integer(25));
        assert_eq!(f.to_string(), "age > 25");
    }

    #[test]
    fn filter_contains_display() {
        let f = Filter::contains(FieldPath::single("email"), "example.com");
        assert_eq!(f.to_string(), "email CONTAINS \"example.com\"");
    }

    #[test]
    fn filter_starts_with_display() {
        let f = Filter::starts_with(FieldPath::single("name"), "J");
        assert_eq!(f.to_string(), "name STARTS WITH \"J\"");
    }

    #[test]
    fn filter_in_display() {
        let f = Filter::in_set(
            FieldPath::single("status"),
            vec![
                DynamicValue::Enum("Active".into()),
                DynamicValue::Enum("Pending".into()),
            ],
        );
        assert_eq!(f.to_string(), "status IN [Active, Pending]");
    }

    #[test]
    fn filter_and_display() {
        let f = Filter::and(vec![
            Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into())),
            Filter::gt(FieldPath::single("age"), DynamicValue::Integer(25)),
        ]);
        assert_eq!(f.to_string(), "(name = \"Jane\" AND age > 25)");
    }

    #[test]
    fn filter_or_display() {
        let f = Filter::or(vec![
            Filter::eq(
                FieldPath::single("status"),
                DynamicValue::Enum("Active".into()),
            ),
            Filter::eq(
                FieldPath::single("status"),
                DynamicValue::Enum("Pending".into()),
            ),
        ]);
        assert_eq!(f.to_string(), "(status = Active OR status = Pending)");
    }

    #[test]
    fn filter_not_display() {
        let f = Filter::negate(Filter::eq(
            FieldPath::single("deleted"),
            DynamicValue::Boolean(true),
        ));
        assert_eq!(f.to_string(), "NOT (deleted = true)");
    }

    #[test]
    fn filter_nested_display() {
        let f = Filter::and(vec![
            Filter::eq(
                FieldPath::parse("company.industry").unwrap(),
                DynamicValue::Text("fintech".into()),
            ),
            Filter::or(vec![
                Filter::gt(FieldPath::single("score"), DynamicValue::Integer(80)),
                Filter::eq(
                    FieldPath::single("priority"),
                    DynamicValue::Enum("high".into()),
                ),
            ]),
        ]);
        assert_eq!(
            f.to_string(),
            "(company.industry = \"fintech\" AND (score > 80 OR priority = high))"
        );
    }

    #[test]
    fn filter_serde_roundtrip_eq() {
        let f = Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into()));
        let json = serde_json::to_string(&f).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn filter_serde_roundtrip_complex() {
        let f = Filter::and(vec![
            Filter::eq(
                FieldPath::parse("company.industry").unwrap(),
                DynamicValue::Text("fintech".into()),
            ),
            Filter::negate(Filter::eq(
                FieldPath::single("deleted"),
                DynamicValue::Boolean(true),
            )),
        ]);
        let json = serde_json::to_string(&f).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    // -- Query tests --

    #[test]
    fn query_basic_display() {
        let schema_id = SchemaId::new();
        let q = Query::new(schema_id.clone());
        let display = q.to_string();
        assert!(display.starts_with("SELECT * FROM "));
        assert!(display.contains(schema_id.as_str()));
    }

    #[test]
    fn query_with_filter_display() {
        let schema_id = SchemaId::new();
        let q = Query::new(schema_id).with_filter(Filter::eq(
            FieldPath::single("name"),
            DynamicValue::Text("Jane".into()),
        ));
        let display = q.to_string();
        assert!(display.contains("WHERE name = \"Jane\""));
    }

    #[test]
    fn query_with_sort_display() {
        let schema_id = SchemaId::new();
        let q = Query::new(schema_id)
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_sort(FieldPath::single("age"), SortOrder::Descending);
        let display = q.to_string();
        assert!(display.contains("ORDER BY name ASC, age DESC"));
    }

    #[test]
    fn query_with_pagination_display() {
        let schema_id = SchemaId::new();
        let q = Query::new(schema_id).with_limit(10).with_offset(20);
        let display = q.to_string();
        assert!(display.contains("LIMIT 10"));
        assert!(display.contains("START 20"));
    }

    #[test]
    fn query_full_display() {
        let schema_id = SchemaId::new();
        let q = Query::new(schema_id)
            .with_filter(Filter::gt(
                FieldPath::single("age"),
                DynamicValue::Integer(25),
            ))
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_limit(10)
            .with_offset(0);
        let display = q.to_string();
        assert!(display.contains("WHERE age > 25"));
        assert!(display.contains("ORDER BY name ASC"));
        assert!(display.contains("LIMIT 10"));
        assert!(display.contains("START 0"));
    }

    #[test]
    fn query_validate_zero_limit() {
        let q = Query::new(SchemaId::new()).with_limit(0);
        assert!(matches!(
            q.validate(),
            Err(QueryError::InvalidLimit { limit: 0 })
        ));
    }

    #[test]
    fn query_validate_valid() {
        let q = Query::new(SchemaId::new()).with_limit(10).with_offset(0);
        assert!(q.validate().is_ok());
    }

    #[test]
    fn query_serde_roundtrip() {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::eq(
                FieldPath::single("name"),
                DynamicValue::Text("Jane".into()),
            ))
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_limit(10)
            .with_offset(5);
        let json = serde_json::to_string(&q).unwrap();
        let back: Query = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn query_serde_skips_defaults() {
        let q = Query::new(SchemaId::new());
        let json = serde_json::to_string(&q).unwrap();
        assert!(!json.contains("filter"));
        assert!(!json.contains("sort"));
        assert!(!json.contains("limit"));
        assert!(!json.contains("offset"));
    }

    // -- QueryError tests --

    #[test]
    fn query_error_display() {
        let cases = vec![
            (QueryError::EmptyFieldPath, "field path must not be empty"),
            (
                QueryError::InvalidFieldPath {
                    path: "a..b".into(),
                    reason: "path contains empty segment".into(),
                },
                "invalid field path 'a..b': path contains empty segment",
            ),
            (
                QueryError::InvalidLimit { limit: 0 },
                "invalid limit 0: must be greater than 0",
            ),
            (
                QueryError::UnknownField {
                    field: "foo".into(),
                    schema: "Contact".into(),
                },
                "unknown field 'foo' in schema 'Contact'",
            ),
            (
                QueryError::TypeMismatch {
                    field: "age".into(),
                    expected: "Integer".into(),
                    actual: "Text".into(),
                },
                "type mismatch for field 'age': expected Integer, got Text",
            ),
            (
                QueryError::EmptyInValues {
                    field: "status".into(),
                },
                "IN filter for field 'status' has no values",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn query_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(QueryError::EmptyFieldPath);
        assert!(err.to_string().contains("field path"));
    }

    // -- validate_filter tests --

    use crate::types::{
        FieldDefinition, FieldModifier, FieldName, IntegerConstraints, SchemaName,
        TextConstraints,
    };

    fn test_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("age").unwrap(),
                    FieldType::Integer(IntegerConstraints::unconstrained()),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn validate_filter_known_field_passes() {
        let schema = test_schema();
        let f = Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into()));
        assert!(validate_filter(&f, &schema).is_ok());
    }

    #[test]
    fn validate_filter_unknown_field_fails() {
        let schema = test_schema();
        let f = Filter::eq(
            FieldPath::single("nonexistent"),
            DynamicValue::Text("x".into()),
        );
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(matches!(&errs[0], QueryError::UnknownField { field, .. } if field == "nonexistent"));
    }

    #[test]
    fn validate_filter_type_mismatch() {
        let schema = test_schema();
        let f = Filter::eq(
            FieldPath::single("age"),
            DynamicValue::Text("not a number".into()),
        );
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, QueryError::TypeMismatch { .. })));
    }

    #[test]
    fn validate_filter_null_always_compatible() {
        let schema = test_schema();
        let f = Filter::eq(FieldPath::single("age"), DynamicValue::Null);
        assert!(validate_filter(&f, &schema).is_ok());
    }

    #[test]
    fn validate_filter_and_propagates() {
        let schema = test_schema();
        let f = Filter::and(vec![
            Filter::eq(FieldPath::single("name"), DynamicValue::Text("Jane".into())),
            Filter::eq(FieldPath::single("bogus"), DynamicValue::Integer(1)),
        ]);
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, QueryError::UnknownField { field, .. } if field == "bogus")));
    }

    #[test]
    fn validate_filter_or_propagates() {
        let schema = test_schema();
        let f = Filter::or(vec![
            Filter::eq(FieldPath::single("missing1"), DynamicValue::Integer(1)),
            Filter::eq(FieldPath::single("missing2"), DynamicValue::Integer(2)),
        ]);
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert_eq!(errs.len(), 2);
    }

    #[test]
    fn validate_filter_not_propagates() {
        let schema = test_schema();
        let f = Filter::negate(Filter::eq(
            FieldPath::single("nope"),
            DynamicValue::Boolean(true),
        ));
        assert!(validate_filter(&f, &schema).is_err());
    }

    #[test]
    fn validate_filter_contains_on_non_text_fails() {
        let schema = test_schema();
        let f = Filter::contains(FieldPath::single("age"), "something");
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, QueryError::TypeMismatch { .. })));
    }

    #[test]
    fn validate_filter_contains_on_text_passes() {
        let schema = test_schema();
        let f = Filter::contains(FieldPath::single("name"), "Jane");
        assert!(validate_filter(&f, &schema).is_ok());
    }

    #[test]
    fn validate_filter_empty_in_values() {
        let schema = test_schema();
        let f = Filter::in_set(FieldPath::single("name"), vec![]);
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, QueryError::EmptyInValues { .. })));
    }

    #[test]
    fn validate_filter_dotted_path_unknown_root() {
        let schema = test_schema();
        let f = Filter::eq(
            FieldPath::parse("unknown.sub").unwrap(),
            DynamicValue::Text("x".into()),
        );
        let errs = validate_filter(&f, &schema).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, QueryError::UnknownField { .. })));
    }

    #[test]
    fn validate_filter_dotted_path_known_root_skips_type_check() {
        let schema = test_schema();
        // Deep path — we only validate root existence, not nested type
        let f = Filter::eq(
            FieldPath::parse("name.sub").unwrap(),
            DynamicValue::Integer(42),
        );
        assert!(validate_filter(&f, &schema).is_ok());
    }

    #[test]
    fn validate_filter_float_field_accepts_integer() {
        // Float fields should accept Integer values (widening)
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Metric").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("score").unwrap(),
                FieldType::Float(crate::types::FloatConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();
        let f = Filter::eq(FieldPath::single("score"), DynamicValue::Integer(42));
        assert!(validate_filter(&f, &schema).is_ok());
    }
}
