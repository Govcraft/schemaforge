//! Bound, transactional account erasure on the remote server.
use crate::SurrealBackend;
use schema_forge_backend::{AccountErasureCounts, BackendError};
use schema_forge_core::types::EntityId;

fn query_error(error: surrealdb::Error) -> BackendError {
    BackendError::QueryError {
        message: error.to_string(),
    }
}

impl SurrealBackend {
    pub(crate) async fn erase_account_transaction(
        &self,
        user: &EntityId,
    ) -> Result<AccountErasureCounts, BackendError> {
        let sql = "BEGIN TRANSACTION; LET $tables = object::keys((INFO FOR DB).tables); LET $account = (SELECT * FROM ONLY $user); LET $identities = (IF $account != NONE AND 'OAuthIdentity' IN $tables { DELETE OAuthIdentity WHERE user = $user RETURN VALUE id } ELSE { [] }); LET $memberships = (IF $account != NONE AND 'TenantMembership' IN $tables { DELETE TenantMembership WHERE user = $user RETURN VALUE id } ELSE { [] }); LET $invitations = (IF $account != NONE AND 'ForgeInvitation' IN $tables { DELETE ForgeInvitation WHERE email = $account.email RETURN VALUE id } ELSE { [] }); DELETE $user; RETURN { identities: array::len($identities), memberships: array::len($memberships), invitations: array::len($invitations) }; COMMIT TRANSACTION;";
        let mut response = self
            .client()
            .query(sql)
            .bind((
                "user",
                surrealdb::types::RecordId::new("User", user.as_str()),
            ))
            .await
            .map_err(query_error)?
            .check()
            .map_err(query_error)?;
        let counts: Option<serde_json::Value> = response.take(7).map_err(query_error)?;
        let counts = counts.ok_or_else(|| BackendError::Internal {
            message: "missing account erasure counts".into(),
        })?;
        serde_json::from_value(counts).map_err(|error| BackendError::Internal {
            message: format!("decode account erasure counts: {error}"),
        })
    }

    pub(crate) async fn delete_invitation_email(&self, email: &str) -> Result<u64, BackendError> {
        let mut response = self.client().query("LET $removed = (DELETE ForgeInvitation WHERE email = $email RETURN VALUE id); RETURN array::len($removed);")
            .bind(("email", email.to_owned())).await.map_err(query_error)?.check().map_err(query_error)?;
        let count: Option<u64> = response.take(1).map_err(query_error)?;
        count.ok_or_else(|| BackendError::Internal {
            message: "missing invitation deletion count".into(),
        })
    }
}
