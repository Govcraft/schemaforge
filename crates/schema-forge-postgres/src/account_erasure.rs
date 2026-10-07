//! Atomic removal of an account and its internal dependants.
use crate::backend::{map_write_error, PgBackend};
use schema_forge_backend::{AccountErasureCounts, BackendError};
use schema_forge_core::types::{EntityId, SchemaName};
use sqlx::{PgConnection, Row};

async fn table_exists(connection: &mut PgConnection, table: &str) -> Result<bool, BackendError> {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("\"{table}\""))
        .fetch_one(connection)
        .await
        .map_err(|error| map_write_error(error, table, "resolve account table"))
}

async fn delete_related(
    connection: &mut PgConnection,
    table: &str,
    column: &str,
    value: &str,
) -> Result<u64, BackendError> {
    if !table_exists(connection, table).await? {
        return Ok(0);
    }
    // Tables and columns are internal constants; account values are always bound.
    // Remove revision markers in the same transaction as their records.
    let query = format!("WITH removed AS (DELETE FROM \"{table}\" WHERE \"{column}\" = $1 RETURNING id), markers AS (DELETE FROM \"_schema_entity_revisions\" WHERE schema_name = $2 AND entity_id IN (SELECT id FROM removed)) SELECT COUNT(*)::BIGINT FROM removed");
    let count: i64 = sqlx::query_scalar(&query)
        .bind(value)
        .bind(table)
        .fetch_one(connection)
        .await
        .map_err(|error| map_write_error(error, table, "erase account related records"))?;
    u64::try_from(count).map_err(|_| BackendError::Internal {
        message: "negative account erasure count".into(),
    })
}

impl PgBackend {
    pub(crate) async fn erase_account_transaction(
        &self,
        user: &EntityId,
    ) -> Result<AccountErasureCounts, BackendError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|error| map_write_error(error, "User", "begin account erasure"))?;
        let row = sqlx::query("SELECT email FROM \"User\" WHERE id = $1 FOR UPDATE")
            .bind(user.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_write_error(error, "User", "lock account for erasure"))?;
        let Some(row) = row else {
            return Ok(AccountErasureCounts::default());
        };
        let email: String = row
            .try_get("email")
            .map_err(|error| map_write_error(error, "User", "read account email"))?;
        let counts = AccountErasureCounts {
            identities: delete_related(&mut tx, "OAuthIdentity", "user", user.as_str()).await?,
            memberships: delete_related(&mut tx, "TenantMembership", "user", user.as_str()).await?,
            invitations: delete_related(&mut tx, "ForgeInvitation", "email", &email).await?,
        };
        sqlx::query("DELETE FROM \"User\" WHERE id = $1")
            .bind(user.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_write_error(error, "User", "erase account"))?;
        Self::remove_revision(
            &mut tx,
            &SchemaName::new("User").map_err(|error| BackendError::Internal {
                message: error.to_string(),
            })?,
            user,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| map_write_error(error, "User", "commit account erasure"))?;
        Ok(counts)
    }

    pub(crate) async fn delete_invitation_email(&self, email: &str) -> Result<u64, BackendError> {
        let mut tx = self.pool().begin().await.map_err(|error| {
            map_write_error(error, "ForgeInvitation", "begin invitation deletion")
        })?;
        let count = delete_related(&mut tx, "ForgeInvitation", "email", email).await?;
        tx.commit().await.map_err(|error| {
            map_write_error(error, "ForgeInvitation", "commit invitation deletion")
        })?;
        Ok(count)
    }
}
