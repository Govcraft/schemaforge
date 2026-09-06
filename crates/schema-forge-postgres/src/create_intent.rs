//! Intent commitment shares the entity/revision transaction. Missing IDs never create.
use crate::backend::PgBackend;
use schema_forge_backend::{
    create_intent::{
        CreateFingerprint, CreateIntentError as Error, CreateIntentId, CreateIntentReceipt,
        CreateIntentRequest, CreateIntentScope, ADMISSION_SECONDS, RECOVERY_SECONDS,
    },
    BackendError,
};
use schema_forge_core::types::EntityId;
use sqlx::{postgres::PgRow, Row};

fn storage(error: sqlx::Error) -> Error {
    BackendError::QueryError {
        message: format!("create receipt storage failed: {error}"),
    }
    .into()
}
fn receipt(row: &PgRow, created: bool) -> Result<CreateIntentReceipt, Error> {
    let id: String = row.try_get("id").map_err(storage)?;
    let entity: Option<String> = row.try_get("entity_id").map_err(storage)?;
    Ok(CreateIntentReceipt {
        id: id.parse()?,
        expires_at: row.try_get("expires_at").map_err(storage)?,
        recover_until: row.try_get("recover_until").map_err(storage)?,
        fingerprint: CreateFingerprint::parse(row.try_get("fingerprint").map_err(storage)?)?,
        definition_matches: true,
        entity_id: entity
            .map(|id| EntityId::parse(&id).map_err(|_| Error::Invalid))
            .transpose()?,
        created,
    })
}
impl PgBackend {
    pub(crate) async fn ensure_create_intents(&self) -> Result<(), BackendError> {
        sqlx::query("CREATE TABLE IF NOT EXISTS _schema_create_intents (id TEXT PRIMARY KEY, principal TEXT NOT NULL, tenant TEXT NOT NULL, schema_name TEXT NOT NULL, schema_id TEXT NOT NULL, definition JSONB NOT NULL, fingerprint TEXT NOT NULL, expires_at TIMESTAMPTZ NOT NULL, recover_until TIMESTAMPTZ NOT NULL, entity_id TEXT)").execute(self.pool()).await.map_err(|e| BackendError::MigrationFailed { step: "create intent receipts".into(), reason: e.to_string() })?;
        sqlx::query("CREATE INDEX IF NOT EXISTS _schema_create_intents_expiry ON _schema_create_intents (recover_until)").execute(self.pool()).await.map_err(|e| BackendError::MigrationFailed { step: "create intent expiry index".into(), reason: e.to_string() })?;
        Ok(())
    }

    async fn check_intent_schema(
        connection: &mut sqlx::PgConnection,
        scope: &CreateIntentScope,
    ) -> Result<(), Error> {
        let definition: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT definition FROM _schema_metadata WHERE name = $1 FOR SHARE")
                .bind(scope.schema.name.as_str())
                .fetch_optional(connection)
                .await
                .map_err(storage)?;
        let expected = serde_json::to_value(&scope.schema).map_err(|_| Error::Invalid)?;
        if definition.as_ref() != Some(&expected) {
            return Err(Error::SchemaChanged);
        }
        Ok(())
    }

    pub(crate) async fn process_create_intent(
        &self,
        request: &CreateIntentRequest,
    ) -> Result<CreateIntentReceipt, Error> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        sqlx::query("SET LOCAL lock_timeout = '3s'")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("SET LOCAL statement_timeout = '4s'")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let result = match request {
            CreateIntentRequest::Reserve { scope, fingerprint } => {
                Self::check_intent_schema(&mut tx, scope).await?;
                // No user may choose a reservation ID, including after pruning.
                let id = CreateIntentId::fresh();
                sqlx::query("DELETE FROM _schema_create_intents WHERE id IN (SELECT id FROM _schema_create_intents WHERE recover_until <= clock_timestamp() LIMIT 100 FOR UPDATE SKIP LOCKED)").execute(&mut *tx).await.map_err(storage)?;
                let row = sqlx::query("INSERT INTO _schema_create_intents (id, principal, tenant, schema_name, schema_id, definition, fingerprint, expires_at, recover_until) VALUES ($1,$2,$3,$4,$5,$6,$7,clock_timestamp()+make_interval(secs=>$8),clock_timestamp()+make_interval(secs=>$9)) RETURNING *")
                    .bind(id.as_str()).bind(&scope.principal).bind(&scope.tenant).bind(scope.schema.name.as_str()).bind(scope.schema.id.as_str()).bind(serde_json::to_value(&scope.schema).map_err(|_| Error::Invalid)?).bind(fingerprint.as_str()).bind(ADMISSION_SECONDS as f64).bind(RECOVERY_SECONDS as f64)
                    .fetch_one(&mut *tx).await.map_err(storage)?;
                receipt(&row, false)?
            }
            CreateIntentRequest::Read { scope, id }
            | CreateIntentRequest::Commit { scope, id, .. } => {
                let row = sqlx::query("SELECT * FROM _schema_create_intents WHERE id=$1 AND principal=$2 AND tenant=$3 AND schema_name=$4 AND schema_id=$5 AND recover_until > clock_timestamp() FOR UPDATE")
                    .bind(id.as_str()).bind(&scope.principal).bind(&scope.tenant).bind(scope.schema.name.as_str()).bind(scope.schema.id.as_str()).fetch_optional(&mut *tx).await.map_err(storage)?.ok_or(Error::Unavailable)?;
                let mut outcome = receipt(&row, false)?;
                let now: chrono::DateTime<chrono::Utc> =
                    sqlx::query_scalar("SELECT clock_timestamp()")
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(storage)?;
                if outcome.recover_until <= now {
                    return Err(Error::Unavailable);
                }
                outcome.definition_matches = row
                    .try_get::<serde_json::Value, _>("definition")
                    .map_err(storage)?
                    == serde_json::to_value(&scope.schema).map_err(|_| Error::Invalid)?;
                if let CreateIntentRequest::Commit {
                    fingerprint,
                    entity,
                    ..
                } = request
                {
                    let stored: String = row.try_get("fingerprint").map_err(storage)?;
                    if stored != fingerprint.as_str() {
                        return Err(Error::ContentConflict);
                    }
                    if outcome.entity_id.is_none() {
                        if outcome.expires_at <= now {
                            return Err(Error::Unavailable);
                        }
                        let definition: serde_json::Value =
                            row.try_get("definition").map_err(storage)?;
                        if definition
                            != serde_json::to_value(&scope.schema).map_err(|_| Error::Invalid)?
                        {
                            return Err(Error::SchemaChanged);
                        }
                        Self::check_intent_schema(&mut tx, scope).await?;
                        if entity.schema != scope.schema.name {
                            return Err(Error::Invalid);
                        }
                        let created =
                            Self::insert_with_revision(&mut tx, entity, Some(&scope.schema))
                                .await?;
                        sqlx::query("UPDATE _schema_create_intents SET entity_id=$2 WHERE id=$1")
                            .bind(id.as_str())
                            .bind(created.entity.id.as_str())
                            .execute(&mut *tx)
                            .await
                            .map_err(storage)?;
                        outcome.entity_id = Some(created.entity.id);
                        outcome.created = true;
                    }
                } else if outcome.entity_id.is_none() && outcome.expires_at <= now {
                    return Err(Error::Unavailable);
                }
                outcome
            }
        };
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }
}
