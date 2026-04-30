# Principal Claims Reference

SchemaForge's Cedar policy engine ships with three intrinsic attributes on
the `Forge::Principal` entity: `id`, `role_rank`, and `roles`. Real-world
authorization frequently needs more — the bearer's organisation, team
membership, customer tier, region, etc. — to support hand-written custom
policies under `policies/custom/`.

The `[schema_forge.authz.principal_claims]` config section maps arbitrary
PASETO `custom` claims onto additional attributes of `Forge::Principal`,
making them available to your custom Cedar policies.

This reference covers the full lifecycle: declaring a mapping, writing
policies that read it, what happens when claims are missing or
malformed, and the restart-required hot-reload limitation.

---

## Table of Contents

1. [When you need this](#1-when-you-need-this)
2. [Configuration syntax](#2-configuration-syntax)
3. [Writing custom policies that read mapped claims](#3-writing-custom-policies-that-read-mapped-claims)
4. [Required vs optional claims](#4-required-vs-optional-claims)
5. [Type vocabulary and validation](#5-type-vocabulary-and-validation)
6. [Hot-reload and restart requirements](#6-hot-reload-and-restart-requirements)
7. [Reserved names and identifier rules](#7-reserved-names-and-identifier-rules)
8. [Worked example: per-org file scoping](#8-worked-example-per-org-file-scoping)

---

## 1. When you need this

The auto-generated tenant guard already handles whole-tenant scoping
(`principal in resource["_tenant"]`). You need this feature when your
domain has **per-record scoping below the tenant level** — for example a
`Workspace` that lives inside a tenant `Firm` but should only be visible
to members of a specific `ClientOrg`.

Without principal-claim mappings, a custom Cedar policy has no way to
say "the resource's `client_org` field must equal the bearer's
`client_org_id` claim". With them, the policy is one rule:

```cedar
forbid (principal, action, resource is Workspace)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
```

---

## 2. Configuration syntax

In your service config (typically `config.toml`), add one section per
attribute under `[schema_forge.authz.principal_claims]`. The TOML key
becomes the Cedar attribute name.

```toml
# Maps PASETO custom claim "client_org_id" to principal.client_org_id (String).
[schema_forge.authz.principal_claims.client_org_id]
type = "string"

# Custom token key, type as a Cedar Set<String>.
[schema_forge.authz.principal_claims.team_ids]
claim = "teams"          # token key (defaults to the section name)
type  = "set_of_string"

# Required claim — a token missing it is rejected with 401 before any
# policy runs.
[schema_forge.authz.principal_claims.tier]
type     = "long"
required = true

# Optional claim with a fallback value when the token omits it.
[schema_forge.authz.principal_claims.region]
type    = "string"
default = "us-east-1"
```

Field reference:

| Field      | Required? | Default         | Notes                                                 |
|------------|-----------|-----------------|-------------------------------------------------------|
| `claim`    | no        | section name    | PASETO `custom` map key.                              |
| `type`     | yes       | —               | One of `string`, `long`, `bool`, `set_of_string`.     |
| `required` | no        | `false`         | Token missing the claim → 401.                        |
| `default`  | no        | none            | Fallback used when `required = false` and absent.     |

If the section is omitted entirely, the runtime behaves identically to
pre-feature deployments — no extra attributes are emitted on
`Forge::Principal` and no operator-supplied claims are read.

---

## 3. Writing custom policies that read mapped claims

Mapped attributes are emitted as **optional** in the generated Cedar
schema. Cedar 4.x's strict-mode validator therefore requires every
reference to be guarded with `principal has X` before any dereference.
This is the safety contract that makes the "skip-when-missing" path
defensible: a policy that drops the guard fails strict validation at
deploy time instead of crashing at request time.

```cedar
// Correct — guarded.
forbid (principal, action, resource is WorkspaceFile)
when {
    principal has client_org_id &&
    resource.client_org != principal.client_org_id
};
```

```cedar
// Wrong — strict-mode validation rejects this at startup.
forbid (principal, action, resource is WorkspaceFile)
when {
    resource.client_org != principal.client_org_id
};
```

The validator's rejection message references the missing attribute on
`Forge::Principal`; add the `has` guard or remove the reference.

---

## 4. Required vs optional claims

The two modes encode different operator intents.

**Optional (`required = false`, the default):** the attribute is
populated when the claim is present (or when a `default` is configured)
and omitted otherwise. Custom policies must defend with
`principal has X`. When the attribute is absent, `principal has X`
returns false and any guarded predicate short-circuits.

**Required (`required = true`):** a token whose `custom` map omits the
claim is rejected before any Cedar policy runs. The adapter raises
`AdapterError::UnrepresentableValue`, which the route layer maps to
**401 Unauthorized**. Use this for claims your policy bundle can't
function without — the operator is declaring "no token without this
claim is well-formed".

Pick `required` when the absence of the claim means the bearer has not
finished their identity provisioning (e.g., they signed in but the IdP
hasn't issued the org assignment yet) — making the request inadmissible
rather than just unauthorized.

---

## 5. Type vocabulary and validation

Four declared types map cleanly onto Cedar values:

| `type`           | Cedar shape       | Required JSON kind                |
|------------------|-------------------|-----------------------------------|
| `string`         | `String`          | JSON string                       |
| `long`           | `Long`            | JSON integer                      |
| `bool`           | `Bool`            | JSON boolean                      |
| `set_of_string`  | `Set<String>`     | JSON array of strings (homogeneous) |

A token claim whose JSON kind doesn't match the declared `type` is
rejected with `AdapterError::UnrepresentableValue` — never silently
coerced. The same rule applies to `set_of_string`: a JSON array
containing anything other than strings is rejected, not partially
populated.

Defaults declared in config are type-checked at config load. A `default`
whose JSON kind doesn't match `type` aborts startup with a clear error
referencing the offending mapping.

An `entity_ref` type (mapping a string claim to a `Forge::Tenant`-style
UID, like `_tenant` is treated today) is intentionally out of scope for
v1 and tracked as a follow-up.

---

## 6. Hot-reload and restart requirements

Out of scope for v1: changes to `[schema_forge.authz.principal_claims]`
require a daemon restart to take effect. Schema mutations (insert /
remove) recompile the policy bundle in place against the *current*
mappings — they do not re-read TOML.

If you change a mapping (rename, change `type`, toggle `required`, edit
`default`), restart the SchemaForge service and re-mint any tokens
whose claims need to satisfy the new contract.

---

## 7. Reserved names and identifier rules

The intrinsic principal attributes — `id`, `role_rank`, `roles` —
cannot be re-declared as mapping names. Attempting to do so aborts
startup with a reserved-name error.

Attribute names must be Cedar identifiers: ASCII letter or underscore
followed by letters, digits, or underscores. Names containing dashes,
spaces, leading digits, or non-ASCII characters are rejected at config
load.

| Allowed         | Rejected           | Reason                          |
|-----------------|--------------------|---------------------------------|
| `client_org_id` | `client-org-id`    | dash is not a Cedar identifier  |
| `Tier`          | `1tier`            | leading digit                   |
| `_internal`     | `with space`       | spaces                          |
| `team_ids`      | `id`               | reserved                        |

---

## 8. Worked example: per-org file scoping

Goal: workspace files are tenant-scoped to a `Firm` (auto-handled by
`@tenant`) and additionally restricted so users can only see files in
their own `ClientOrg` within the firm.

**Config** (`config.toml`):

```toml
[schema_forge.authz.principal_claims.client_org_id]
type     = "string"
required = true     # every token in this tenant must carry the org id
```

**Token issuance** (your IdP / login service):

```json
{
  "sub": "user:alice",
  "roles": ["editor"],
  "tenant_chain": [{ "schema": "Firm", "entity_id": "firm-acme" }],
  "client_org_id": "org-42"
}
```

**Custom Cedar policy** (`policies/custom/per_org_files.cedar`):

```cedar
forbid (principal, action, resource is WorkspaceFile)
when {
    principal has client_org_id &&
    resource has client_org &&
    resource.client_org != principal.client_org_id
};
```

**Behavior at request time:**

- `GET /WorkspaceFile/{id}` where the file's `client_org == "org-42"`:
  the `forbid` does not fire; the request reaches whatever `permit` the
  `@access` annotations or other custom policies provide. → 200 (or
  whatever access otherwise grants).
- `GET /WorkspaceFile/{id}` where the file's `client_org == "org-13"`:
  the `forbid` fires. → 403.
- A token missing `client_org_id` entirely: rejected by the adapter
  before policy evaluation. → 401.

The intrinsic tenant guard still runs alongside this rule, so a request
crossing a tenant boundary is rejected even if the org id matches by
coincidence.
