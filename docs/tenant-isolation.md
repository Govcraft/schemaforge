# Tenant isolation

Only schemas with `@tenant(root)` or `@tenant(parent: "...")` receive automatic tenant filters and tenant stamps. Shared schemas remain accessible according to their Cedar policies, including when referenced from tenant-owned rows.

A root row's identity defines its tenant. New root rows store `_tenant = id`; authorization and list scoping derive that value from `id` for existing roots too. Legacy roots with NULL or inconsistent `_tenant` metadata therefore remain accessible to their own members without exposing other roots or requiring a data rewrite.

A tenant-owned child must carry valid `_tenant` metadata. Generated Cedar policies deny reads, updates and deletes of missing or NULL child tenants for non-platform administrators. Apply regenerated policies when upgrading a deployment that persists generated Cedar files. The canonical query filter and PostgreSQL authorized counts exclude these unstamped child rows.

Tenant members cannot move rows by supplying `_tenant` in PUT or PATCH: the server removes that input and preserves stored ownership. Platform administrators can reassign child rows. Root identities remain immutable for every caller.

Invitation tenant targets require a configured tenant schema and a type/id pair in the caller's effective tenant chain. Active-tenant narrowing applies before delegation; platform administrators may delegate across tenants.
