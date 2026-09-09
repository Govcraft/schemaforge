# Enum migrations preserve valid values

Adding variants or reordering an enum uses an identity transform. Existing values
remain unchanged, and the migration is classified as safe.

Removing variants requires confirmation. The plan lists the removed variants in
`null_removed_enum_variants(...)`; only rows containing those values are nulled.
Rows containing retained variants remain unchanged. PostgreSQL and SurrealDB
update constraints together with the data in a migration transaction.

A required field cannot be nulled. Migrate values to retained variants before
narrowing a required enum, or make the field optional in a separate migration.
PostgreSQL and SurrealDB reject affected required rows and roll back. SQL Server
rejects a narrowing transform on a required field before updating rows.
