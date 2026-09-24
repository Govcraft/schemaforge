# Safe schema changes

## Rename a field without losing its values

Declare the previous field name on the replacement field:

```schema
schema Line {
    business_number: text required @renamed_from("number")
}
```

`apply`, `migrate`, and `serve` use the hint to rename the existing column.
It remains valid after the rename and can stay in signed schema artifacts.
The old name must identify exactly one removed field. A source and destination
that both already exist, a missing source and destination, duplicate sources,
and self-renames are rejected before migration. PostgreSQL also renames generated indexes and CHECK/foreign-key constraints,
allowing later modifier and enum changes. SurrealDB preserves the full field
definition during its transactional copy. SQL Server renames keys in its tagged
JSON payloads; all steps execute in one transaction and destination collisions
roll back the whole plan. Context-free SurrealDB SQL generation refuses renames
because preserving constraints requires stored schema metadata.

Without this declaration, changing a field name means removing one field and
adding another. `apply` and `migrate --execute` require confirmation for that
operation. `serve` refuses destructive startup plans before applying any of the
user's schema plans. After reviewing the plan, use
`serve --allow-destructive-migrations` to explicitly accept data loss.
Admin schema PUT requests must include `"allow_destructive_migrations": true`
for destructive changes. Omitted or false values refuse the operation. Lossy
type conversions and enum variant removal also count as destructive.

Runtime requests preserve omitted schema and field annotations. Explicit
annotation arrays replace them; declared tenancy transitions are rejected.
Creating or deleting tenanted schemas and changing their parent-reference
structure require offline apply and restart, because the actor and HTTP
middleware must activate the same tenant configuration together. Invalid
combined hierarchies are rejected before schema writes.

Runtime schema changes validate the complete Cedar bundle before storage and
install that exact immutable bundle after storage succeeds. Concurrent changes
with an outdated preflight return HTTP 409 and must be retried. Storage failures
preserve the active registry and policy bundle, but DDL, metadata, and final
constraint reconciliation can use separate backend transactions. A later failure
does not undo earlier committed database changes. Inspect the reported failure
and stored schema before retrying; use a maintenance window for migrations.

## PostgreSQL relation integrity

To-one relations use foreign keys to the target table's `id`, with restrictive
deletion behavior. Table creation and later relation additions follow the same
policy. Constraint installation is deferred until the referenced table exists,
so file order and cyclic schemas work. Schema administration finishes with an
idempotent reconciliation pass over stored metadata. `apply`,
`migrate --execute`, and startup also repair legacy missing constraints even
when the schema text has no changes. Read commands and connections do not run
this repair. Dry runs do not install constraints; they do not inspect existing
rows for orphan references.

Finalization fails if a target table is missing. An orphaned value makes
constraint installation fail with the source schema and field; repair or remove
that orphan and rerun the operation. Already completed migrations are not
rolled back across the whole batch. Run migrations during a maintenance window:
adding a validated foreign key scans existing rows and takes PostgreSQL locks.

Writing a nonexistent related ID or deleting a referenced record returns HTTP
409 `foreign_key_violation`, with schema and constraint names. It does not
return raw SQL or row values. Existing foreign keys with default `NO ACTION`
remain valid: these are nondeferrable and prevent referenced deletions as well.

## Changing tenancy requires an explicit data migration

Adding, removing, or changing `@tenant` on a stored schema is rejected before
automatic DDL or metadata replacement. `--force` and the destructive startup
opt-in do not bypass this check. A schema annotation cannot establish which
tenant owns each existing row. Leaving old rows unattributed or guessing an
owner would change authorization and unique constraints without a sound data
mapping.

For PostgreSQL, use this procedure with the service and other writers offline.
If a systemd restart policy is configured, mask the service for the maintenance
window. Take a database backup and test the procedure against a restored copy.
Perform tenancy changes separately from field changes.

1. Apply the complete desired schema set to an empty staging database. Start the staging server once to validate the desired hierarchy. The staging
   database provides canonical metadata and DDL.
   Compare the staging table's columns, indexes, and constraints with production.
2. Prepare an explicit mapping from every affected row ID to a valid tenant ID
   in the desired hierarchy. Verify completeness and tenant existence. `_tenant`
   stores the tenant entity ID, without a schema-name prefix. Update token tenant
   chains and related tables when changing the hierarchy.
3. In one production transaction, lock affected tables, change the physical
   columns/constraints, backfill ownership, verify all invariants, and copy only
   the desired annotation array into the existing metadata row. Preserve the
   production schema ID and fields. Roll back on any failed check.
4. Unmask any service masked for maintenance, then restart using the matching
   desired schema files. Verify tenant-isolated reads
   and writes with real tenant principals, and only then restore normal service.

The following example converts a global `Contact.phone unique` schema to
`@tenant(parent: "Org")`. It assumes Org already exists as a tenant root and
all Contact rows belong to **one explicitly chosen Org**. For multiple tenants,
replace the UPDATE with a join against your reviewed row-to-tenant mapping.

First export the canonical annotations from the staging database that has the
desired definition:

```sh
psql "$STAGING_DATABASE_URL" -X -Atc \
  "SELECT definition::jsonb->'annotations' FROM \"_schema_metadata\" WHERE name = 'Contact'" \
  > contact-annotations.json
psql "$PRODUCTION_DATABASE_URL" -X -v ON_ERROR_STOP=1 \
  --set=annotations="$(cat contact-annotations.json)" \
  --set=tenant_id='REPLACE_WITH_VERIFIED_ORG_ID' -f contact-tenancy.sql
```

Contents of `contact-tenancy.sql`:

```sql
BEGIN;
LOCK TABLE "Contact", "Org", "_schema_metadata" IN ACCESS EXCLUSIVE MODE;
ALTER TABLE "Contact" ADD COLUMN "_tenant" TEXT;
UPDATE "Contact" SET "_tenant" = :'tenant_id';
DO $$ BEGIN
  IF EXISTS (
    SELECT 1 FROM "Contact" c LEFT JOIN "Org" o ON c."_tenant" = o.id
    WHERE c."_tenant" IS NULL OR o.id IS NULL
  ) THEN RAISE EXCEPTION 'tenant attribution incomplete or invalid'; END IF;
END $$;
ALTER TABLE "Contact" DROP CONSTRAINT "uq_Contact_phone";
CREATE INDEX "idx_Contact_tenant" ON "Contact" ("_tenant");
CREATE UNIQUE INDEX "uq_Contact_phone" ON "Contact" ("_tenant", "phone");
UPDATE "_schema_metadata"
SET definition = jsonb_set(definition::jsonb, '{annotations}', :'annotations'::jsonb)
WHERE name = 'Contact';
COMMIT;
```

Check that exactly one metadata row was updated. The annotation file must come
from the validated desired staging schema, not a hand-invented serialization.
Repeat the unique-constraint conversion for every unique field.

Other transitions have different requirements:

- **Adding a root:** add `_tenant` and its index, then set `_tenant = id` on
  every root row. Root unique fields stay global.
  Establish a valid single-root hierarchy and reviewed ownership values; root
  rows represent tenant boundaries and must not be assigned to arbitrary roots.
- **Removing tenancy from a child:** verify and resolve duplicate values across
  tenants, drop each composite unique index, create global unique constraints,
  then drop `_tenant` (its supporting index is dropped with the column). Removing
  isolation changes who can read records; review the resulting policies before
  accepting the change. Removing a root also requires updating its descendants.
- **Changing child parent:** keep `_tenant` and composite unique indexes, but
  remap every affected row to a valid entity in the new hierarchy. Update
  descendants and principal tenant chains together.
- **Root to child:** establish a different valid root, backfill parent ownership,
  and replace global unique constraints with composite indexes.
- **Child to root:** establish a valid single-root hierarchy, set `_tenant = id`
  on every root row, resolve cross-tenant duplicates, and replace composite
  indexes with global unique constraints.

For every case, commit physical changes, validated ownership, and the canonical
annotation update together. Other database backends require equivalent native
DDL and metadata changes; the PostgreSQL SQL above is not portable.
