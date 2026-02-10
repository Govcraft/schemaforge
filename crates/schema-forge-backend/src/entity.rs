use std::collections::BTreeMap;

use schema_forge_core::types::{DynamicValue, EntityId, SchemaName};

/// A runtime entity: a record in a schema-defined table.
///
/// Fields are stored as a `BTreeMap` for deterministic ordering,
/// which simplifies testing and serialization.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    /// The unique entity identifier (TypeID with "entity" prefix).
    pub id: EntityId,
    /// The schema this entity belongs to.
    pub schema: SchemaName,
    /// Field name to value mapping.
    pub fields: BTreeMap<String, DynamicValue>,
}

impl Entity {
    /// Creates a new entity with the given schema and fields.
    /// Generates a fresh `EntityId`.
    pub fn new(schema: SchemaName, fields: BTreeMap<String, DynamicValue>) -> Self {
        Self {
            id: EntityId::new(),
            schema,
            fields,
        }
    }

    /// Creates an entity with a specific ID (used when loading from storage).
    pub fn with_id(
        id: EntityId,
        schema: SchemaName,
        fields: BTreeMap<String, DynamicValue>,
    ) -> Self {
        Self { id, schema, fields }
    }

    /// Returns the value of a field by name, if present.
    pub fn field(&self, name: &str) -> Option<&DynamicValue> {
        self.fields.get(name)
    }

    /// Returns the number of fields.
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

impl std::fmt::Display for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.schema, self.id)
    }
}

/// The result of a query execution: a list of entities with optional total count.
///
/// When `total_count` is `Some`, it represents the total number of matching entities
/// before pagination (LIMIT/OFFSET) was applied. This is useful for building pagination UIs.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    /// The entities returned by the query.
    pub entities: Vec<Entity>,
    /// The total count of matching entities before pagination, if available.
    pub total_count: Option<usize>,
}

impl QueryResult {
    /// Creates a new query result.
    pub fn new(entities: Vec<Entity>, total_count: Option<usize>) -> Self {
        Self {
            entities,
            total_count,
        }
    }

    /// Returns true if no entities were returned.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Returns the number of entities returned.
    pub fn len(&self) -> usize {
        self.entities.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_schema_name() -> SchemaName {
        SchemaName::new("Contact").unwrap()
    }

    fn make_fields() -> BTreeMap<String, DynamicValue> {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".into()));
        fields.insert("age".to_string(), DynamicValue::Integer(30));
        fields
    }

    #[test]
    fn entity_new_generates_id() {
        let entity = Entity::new(make_schema_name(), make_fields());
        assert!(entity.id.as_str().starts_with("entity_"));
        assert_eq!(entity.schema.as_str(), "Contact");
        assert_eq!(entity.field_count(), 2);
    }

    #[test]
    fn entity_with_id_preserves_id() {
        let id = EntityId::new();
        let entity = Entity::with_id(id.clone(), make_schema_name(), make_fields());
        assert_eq!(entity.id, id);
    }

    #[test]
    fn entity_field_access() {
        let entity = Entity::new(make_schema_name(), make_fields());
        assert_eq!(
            entity.field("name"),
            Some(&DynamicValue::Text("Alice".into()))
        );
        assert_eq!(entity.field("age"), Some(&DynamicValue::Integer(30)));
        assert_eq!(entity.field("missing"), None);
    }

    #[test]
    fn entity_display() {
        let entity = Entity::new(make_schema_name(), BTreeMap::new());
        let display = entity.to_string();
        assert!(display.starts_with("Contact:entity_"));
    }

    #[test]
    fn query_result_empty() {
        let result = QueryResult::new(vec![], None);
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert_eq!(result.total_count, None);
    }

    #[test]
    fn query_result_with_entities() {
        let entities = vec![
            Entity::new(make_schema_name(), make_fields()),
            Entity::new(make_schema_name(), make_fields()),
        ];
        let result = QueryResult::new(entities, Some(10));
        assert!(!result.is_empty());
        assert_eq!(result.len(), 2);
        assert_eq!(result.total_count, Some(10));
    }
}
