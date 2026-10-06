# Tenant isolation

When a tenant root exists, every application schema must declare `@tenant(root)` or `@tenant(parent: "...")`. Validation rejects unannotated application schemas before apply or startup; system schemas remain exempt. Projects without a tenant root continue to support unannotated schemas. Existing projects must annotate previously unscoped application schemas and assign valid ownership to existing rows before serving them.

A root row's identity defines its tenant. New root rows store `_tenant = id`; authorization and list scoping derive that value from `id` for existing roots too. Legacy roots with NULL or inconsistent `_tenant` metadata therefore remain accessible to their own members without exposing other roots or requiring a data rewrite.

A tenant-owned child must carry valid `_tenant` metadata. Generated Cedar policies deny reads, updates and deletes of missing or NULL child tenants for non-platform administrators. The server regenerates these policies from registered schemas at startup. The canonical query filter and PostgreSQL authorized counts exclude these unstamped child rows.

Tenant members cannot move rows by supplying `_tenant` in PUT or PATCH: the server removes that input and preserves stored ownership. Platform administrators can reassign child rows. Root identities remain immutable for every caller.

Invitation tenant targets require a configured tenant schema and a type/id pair in the caller's effective tenant chain. Active-tenant narrowing applies before delegation; platform administrators may delegate across tenants.

Relation writes are checked after rules and hooks against tenant scope and read authorization. Missing and inaccessible targets return the same validation error. This applies to single and collection relations on create, PUT and PATCH. Platform administrators retain cross-tenant access.

An operator can grant each authenticated root creator a scoped membership:

```toml
[schema_forge.tenancy]
creator_role = "owner"
```

Declare the role in `policies/role_ranks.toml` and use it in the root schema's
permissions. Root creation and creator membership commit atomically. This grants
permissions within that tenant; it does not grant a global User role. Refresh
the session after creation to load the new membership and role, then select
the new tenant through the active-tenant header when multiple memberships exist.
Omitting
`creator_role` preserves explicit membership administration. Automatic grants
reject empty or unknown roles and `platform_admin`. Refresh the token after root
creation, then select the new root with `X-Active-Tenant`. Signed membership
roles augment account roles only for the selected tenant; other memberships
do not contribute roles to that request.

Open OAuth signup also needs a default root that already exists. Users without
membership cannot complete a tenant login, so `creator_role` alone cannot solve
first-login onboarding. Configure the default after applying and seeding the root:

```toml
[schema_forge.tenancy.default_tenant]
schema = "Organization"
id = "organization_01k00000000000000000000000"
role = "member"
```

Replace the example ID with the existing entity's TypeID. `role` defaults to
`member` and is independent of OAuth `default_roles`. The server validates the
root schema, registered role and entity existence before applying application
schema migrations. Enabled open OAuth signup with tenancy refuses startup when
this default is missing. Signed invitation membership takes precedence.
