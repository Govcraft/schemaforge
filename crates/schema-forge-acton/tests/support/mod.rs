//! A deliberately limited immutable fixture for portable HTTP read tests.
//!
//! Real database suites cover writes, constraints, transactions, and concurrency.
//! Unsupported operations fail explicitly so this fixture cannot silently stand
//! in for a database's semantics.

use schema_forge_backend::{BackendError, Entity, EntityStore, QueryResult, SchemaBackend};
use schema_forge_core::{
    migration::MigrationStep,
    query::{AggregateQuery, AggregateResult, Query},
    types::{EntityId, SchemaDefinition, SchemaName},
};

pub struct FixtureBackend {
    schema: SchemaDefinition,
    entities: Vec<Entity>,
}

impl FixtureBackend {
    pub fn new(schema: SchemaDefinition, entities: Vec<Entity>) -> Self {
        assert!(entities.iter().all(|entity| entity.schema == schema.name));
        Self { schema, entities }
    }

    fn query_entities(&self, query: &Query) -> Result<&[Entity], BackendError> {
        if query.schema != self.schema.id
            || query.filter.is_some()
            || !query.sort.is_empty()
            || query.projection.is_some()
        {
            return Err(unsupported(
                "filtered, sorted, projected, or unknown-schema query",
            ));
        }
        Ok(&self.entities)
    }
}

fn unsupported(operation: &str) -> BackendError {
    BackendError::QueryError {
        message: format!("immutable HTTP fixture does not support {operation}"),
    }
}

impl SchemaBackend for FixtureBackend {
    async fn apply_migration(
        &self,
        _: &SchemaName,
        _: &[MigrationStep],
    ) -> Result<(), BackendError> {
        Err(unsupported("migrations"))
    }

    async fn store_schema_metadata(&self, _: &SchemaDefinition) -> Result<(), BackendError> {
        Err(unsupported("schema writes"))
    }

    async fn load_schema_metadata(
        &self,
        name: &SchemaName,
    ) -> Result<Option<SchemaDefinition>, BackendError> {
        Ok((name == &self.schema.name).then(|| self.schema.clone()))
    }

    async fn list_schema_metadata(&self) -> Result<Vec<SchemaDefinition>, BackendError> {
        Ok(vec![self.schema.clone()])
    }
}

impl EntityStore for FixtureBackend {
    async fn create(&self, _: &Entity) -> Result<Entity, BackendError> {
        Err(unsupported("creates"))
    }

    async fn get(&self, schema: &SchemaName, id: &EntityId) -> Result<Entity, BackendError> {
        self.entities
            .iter()
            .find(|entity| &entity.schema == schema && &entity.id == id)
            .cloned()
            .ok_or_else(|| BackendError::EntityNotFound {
                schema: schema.to_string(),
                entity_id: id.to_string(),
            })
    }

    async fn update(&self, _: &Entity) -> Result<Entity, BackendError> {
        Err(unsupported("updates"))
    }

    async fn delete(&self, _: &SchemaName, _: &EntityId) -> Result<(), BackendError> {
        Err(unsupported("deletes"))
    }

    async fn query(&self, query: &Query) -> Result<QueryResult, BackendError> {
        let entities = self.query_entities(query)?;
        Ok(QueryResult::new(
            entities
                .iter()
                .skip(query.offset.unwrap_or(0))
                .take(query.limit.unwrap_or(usize::MAX))
                .cloned()
                .collect(),
            query.include_total.then_some(entities.len()),
        ))
    }

    async fn count(&self, query: &Query) -> Result<usize, BackendError> {
        Ok(self.query_entities(query)?.len())
    }

    async fn aggregate(&self, _: &AggregateQuery) -> Result<Vec<AggregateResult>, BackendError> {
        Err(unsupported("aggregates"))
    }
}
