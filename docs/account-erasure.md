# Account erasure

`DELETE /api/v1/forge/users/{username}` erases the stored account and its system records in one database transaction on PostgreSQL, SurrealDB and SQL Server:

1. OAuth identities referencing the account.
2. Tenant memberships referencing the account.
3. Invitations whose email exactly matches the stored account email, including pending, consumed and revoked invitations.
4. The User record.

The endpoint retains Cedar authorization, the self-deletion restriction and the last-platform-administrator restriction. Operator-defined references to User remain protected. If a database rejects any deletion, every deletion rolls back and the account remains intact. Unrelated accounts and their records remain unchanged. Legacy deployments without the optional identity, membership or invitation tables can still erase accounts.

A successful request returns 204. The `forge.user.deleted` audit event records the `identities`, `memberships` and `invitations` counts along with the existing actor and target metadata. Administrator audit browsing exposes the counts without including invitation tokens or external provider subjects. Counts are emitted only after the database transaction commits. Existing asynchronous audit delivery guarantees still apply.

Custom authentication or entity-storage adapters must implement atomic account erasure. The default implementation refuses the operation before making any change, rather than performing partial deletion. The existing `AuthStore::delete_user` signature is retained; the built-in store also provides `erase_user` when counts are needed. `InviteStore::delete_by_email` provides bulk invitation removal, while account erasure deletes invitations within its own transaction.
