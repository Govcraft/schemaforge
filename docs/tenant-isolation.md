# Tenant isolation

When a tenant root exists, every application schema must declare `@tenant(root)` or `@tenant(parent: "...")`. Validation rejects unannotated application schemas before apply or startup; system schemas remain exempt. Projects without a tenant root continue to support unannotated schemas. Existing projects must annotate previously unscoped application schemas and assign valid ownership to existing rows before serving them.

A root row's identity defines its tenant. New root rows store `_tenant = id`; authorization and list scoping derive that value from `id` for existing roots too. Legacy roots with NULL or inconsistent `_tenant` metadata therefore remain accessible to their own members without exposing other roots or requiring a data rewrite.

A tenant-owned child must carry valid `_tenant` metadata. Generated Cedar policies deny reads, updates and deletes of missing or NULL child tenants for non-platform administrators. The server regenerates these policies from registered schemas at startup. The canonical query filter and PostgreSQL authorized counts exclude these unstamped child rows.

Tenant members cannot move rows by supplying `_tenant` in PUT or PATCH: the server removes that input and preserves stored ownership. Platform administrators can reassign child rows. Root identities remain immutable for every caller.

Invitation tenant targets require a configured tenant schema and a type/id pair in the caller's effective tenant chain. Active-tenant narrowing applies before delegation; platform administrators may delegate across tenants.

Relation writes are checked after rules and hooks against tenant scope and read authorization. Missing and inaccessible targets return the same validation error. This applies to single and collection relations on create, PUT and PATCH. Platform administrators retain cross-tenant access.
