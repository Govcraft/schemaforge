//! PostgreSQL record revisions. Entity and marker changes share one transaction.

use schema_forge_backend::conditional::{
    ConditionalMutationError, EntityRevision, VersionedEntity,
};
use schema_forge_backend::{BackendError, Entity};
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};
use sqlx::PgConnection;

use crate::backend::PgBackend;
use crate::value::row_to_entity;

pub(crate) const REVISIONS: &str = "_schema_entity_revisions";
const READY: &str = "_schema_revision_ready";

fn database_error(error: sqlx::Error) -> BackendError {
    BackendError::QueryError {
        message: format!("record revision storage failed: {error}"),
    }
}

impl PgBackend {
    pub(crate) async fn ensure_revision_tables(&self) -> Result<(), BackendError> {
        for sql in [
            format!("CREATE TABLE IF NOT EXISTS \"{REVISIONS}\" (schema_name TEXT NOT NULL, entity_id TEXT NOT NULL, revision TEXT NOT NULL, PRIMARY KEY (schema_name, entity_id))"),
            format!("CREATE TABLE IF NOT EXISTS \"{READY}\" (schema_name TEXT PRIMARY KEY)"),
        ] {
            sqlx::query(&sql).execute(self.pool()).await.map_err(database_error)?;
        }
        Ok(())
    }

    /// Explicit schema-apply checkpoint. Readers never backfill records.
    pub(crate) async fn prepare_revisions(&self, schema: &SchemaName) -> Result<(), BackendError> {
        let mut tx = self.pool().begin().await.map_err(database_error)?;
        // Coordinate with every writer, including a writer creating a new row.
        sqlx::query(&format!(
            "LOCK TABLE \"{}\" IN SHARE ROW EXCLUSIVE MODE",
            schema.as_str()
        ))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        let ids: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT e.id FROM \"{}\" e LEFT JOIN \"{REVISIONS}\" r ON r.schema_name = $1 AND r.entity_id = e.id WHERE r.entity_id IS NULL", schema.as_str()))
            .bind(schema.as_str()).fetch_all(&mut *tx).await.map_err(database_error)?;
        for id in ids {
            let revision = EntityRevision::fresh();
            sqlx::query(&format!("INSERT INTO \"{REVISIONS}\" (schema_name, entity_id, revision) VALUES ($1, $2, $3)"))
                .bind(schema.as_str()).bind(id).bind(revision.as_str())
                .execute(&mut *tx).await.map_err(database_error)?;
        }
        sqlx::query(&format!(
            "INSERT INTO \"{READY}\" (schema_name) VALUES ($1) ON CONFLICT DO NOTHING"
        ))
        .bind(schema.as_str())
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)
    }

    /// DDL/backfills can change stored values too. Invalidate old baselines in
    /// the same migration transaction; a dropped table loses readiness.
    pub(crate) async fn invalidate_schema_revisions(
        connection: &mut PgConnection,
        schema: &SchemaName,
    ) -> Result<(), BackendError> {
        sqlx::query(&format!(
            "DELETE FROM \"{REVISIONS}\" WHERE schema_name = $1"
        ))
        .bind(schema.as_str())
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema = current_schema() AND table_name = $1)")
            .bind(schema.as_str()).fetch_one(&mut *connection).await.map_err(database_error)?;
        if exists {
            let ids: Vec<String> =
                sqlx::query_scalar(&format!("SELECT id FROM \"{}\"", schema.as_str()))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(database_error)?;
            for id in ids {
                let revision = EntityRevision::fresh();
                sqlx::query(&format!("INSERT INTO \"{REVISIONS}\" (schema_name, entity_id, revision) VALUES ($1, $2, $3)"))
                    .bind(schema.as_str()).bind(id).bind(revision.as_str())
                    .execute(&mut *connection).await.map_err(database_error)?;
            }
        } else {
            sqlx::query(&format!("DELETE FROM \"{READY}\" WHERE schema_name = $1"))
                .bind(schema.as_str())
                .execute(connection)
                .await
                .map_err(database_error)?;
        }
        Ok(())
    }

    pub(crate) async fn advance_revision(
        connection: &mut PgConnection,
        entity: &Entity,
    ) -> Result<EntityRevision, BackendError> {
        let revision = EntityRevision::fresh();
        sqlx::query(&format!("INSERT INTO \"{REVISIONS}\" (schema_name, entity_id, revision) VALUES ($1, $2, $3) ON CONFLICT (schema_name, entity_id) DO UPDATE SET revision = EXCLUDED.revision"))
            .bind(entity.schema.as_str()).bind(entity.id.as_str()).bind(revision.as_str())
            .execute(connection).await.map_err(database_error)?;
        Ok(revision)
    }

    pub(crate) async fn remove_revision(
        connection: &mut PgConnection,
        schema: &SchemaName,
        id: &EntityId,
    ) -> Result<(), BackendError> {
        sqlx::query(&format!(
            "DELETE FROM \"{REVISIONS}\" WHERE schema_name = $1 AND entity_id = $2"
        ))
        .bind(schema.as_str())
        .bind(id.as_str())
        .execute(connection)
        .await
        .map_err(database_error)?;
        Ok(())
    }

    /// Avoid loading metadata or opening a transaction for unprepared schemas.
    /// Do not cache: preparation can be performed by a separate CLI process.
    pub(crate) async fn check_revision_readiness(
        &self,
        schema: &SchemaName,
    ) -> Result<(), ConditionalMutationError> {
        let mut connection = self.pool().acquire().await.map_err(database_error)?;
        Self::require_revision_ready(&mut connection, schema).await
    }

    async fn require_revision_ready(
        connection: &mut PgConnection,
        schema: &SchemaName,
    ) -> Result<(), ConditionalMutationError> {
        let ready: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM \"{READY}\" WHERE schema_name = $1)"
        ))
        .bind(schema.as_str())
        .fetch_one(connection)
        .await
        .map_err(database_error)?;
        if ready {
            Ok(())
        } else {
            Err(ConditionalMutationError::Unsupported)
        }
    }

    async fn read_revision(
        connection: &mut PgConnection,
        schema: &SchemaName,
        id: &EntityId,
    ) -> Result<EntityRevision, ConditionalMutationError> {
        let marker: Option<String> = sqlx::query_scalar(&format!(
            "SELECT r.revision FROM \"{REVISIONS}\" r JOIN \"{READY}\" ready ON ready.schema_name = r.schema_name WHERE r.schema_name = $1 AND r.entity_id = $2"
        ))
        .bind(schema.as_str())
        .bind(id.as_str())
        .fetch_optional(connection)
        .await
        .map_err(database_error)?;
        // Readiness must still hold in this snapshot. A schema can be dropped
        // and recreated between the cheap initial check and the entity read.
        // Missing markers are never silently initialized on reads or conditional writes.
        marker
            .ok_or(ConditionalMutationError::Unsupported)?
            .parse()
            .map_err(|_| {
                ConditionalMutationError::Backend(BackendError::Internal {
                    message: "invalid stored record revision".into(),
                })
            })
    }

    pub(crate) async fn read_versioned(
        &self,
        schema: &SchemaName,
        id: &EntityId,
        definition: Option<&SchemaDefinition>,
    ) -> Result<VersionedEntity, ConditionalMutationError> {
        let mut tx = self.pool().begin().await.map_err(database_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        let row = sqlx::query(&format!(
            "SELECT * FROM \"{}\" WHERE id = $1",
            schema.as_str()
        ))
        .persistent(false)
        .bind(id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| BackendError::EntityNotFound {
            schema: schema.to_string(),
            entity_id: id.to_string(),
        })?;
        let entity = row_to_entity(&row, schema, definition)?;
        let revision = Self::read_revision(&mut tx, schema, id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(VersionedEntity { entity, revision })
    }

    pub(crate) async fn lock_expected(
        connection: &mut PgConnection,
        schema: &SchemaName,
        id: &EntityId,
        expected: &EntityRevision,
    ) -> Result<(), ConditionalMutationError> {
        Self::require_revision_ready(connection, schema).await?;
        // This lock is shared with ordinary UPDATE/DELETE lock ordering. A
        // competing writer cannot commit a row change without its new marker.
        let row = sqlx::query(&format!(
            "SELECT id FROM \"{}\" WHERE id = $1 FOR UPDATE",
            schema.as_str()
        ))
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?;
        if row.is_none() {
            return Err(ConditionalMutationError::Conflict);
        }
        if Self::read_revision(connection, schema, id).await? != *expected {
            return Err(ConditionalMutationError::Conflict);
        }
        Ok(())
    }
}
