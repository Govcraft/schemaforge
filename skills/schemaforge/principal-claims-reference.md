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

---

## 9. IN-side: projecting User columns into the token at login

Sections 1-8 cover the **OUT-side** (token → Cedar attribute). The
companion **IN-side** is how the daemon *populates* a custom claim at
login time by reading a column off the User entity row, so a deployment
that declares `required = true` doesn't 401 every login.

### 9.1 Syntax

Add an optional `source` block to the mapping:

```toml
[schema_forge.authz.principal_claims.client_org_id]
type     = "string"
required = true
source   = { user_field = "client_org_id" }   # NEW (issue #51)
```

`source = { user_field = "<f>" }` means: at every `/auth/login` and
`/auth/refresh`, read the named column off the User entity row and write
its value into the PASETO `custom.<claim>` map. The OUT-side adapter
then projects it onto `Forge::Principal` as before — both ends of the
loop are now closed inside the daemon.

A mapping with no `source` keeps the pre-#51 behaviour: the bearer is
expected to supply the claim out-of-band (CLI-issued token, external
IdP, etc.).

### 9.2 Field-type → claim-type projection table

The DSL field type on the User schema and the declared `type` on the
mapping must form one of the rows below. Anything else aborts startup
with a `principal claim '<name>': source.user_field '<f>'` error.

| User field type     | Declared `type`     | Projection                          |
|---------------------|---------------------|-------------------------------------|
| `text`              | `string`            | as-is                               |
| `integer`           | `long`              | as-is                               |
| `boolean`           | `bool`              | as-is                               |
| `text[]`            | `set_of_string`     | as-is                               |
| `-> Target` (one)   | `string`            | target entity id (string)           |
| `-> Target[]` (many)| `set_of_string`     | set of target entity ids            |

**Rejected at config load:** `richtext`, `json`, `file`, `datetime`,
`enum`, `composite`, `integer[]`, and float types. These have no
canonical lossless projection — instead of guessing, the daemon refuses
to start. (For `datetime` and `enum`, declare an explicit string-typed
helper column on User if you need them; that's an open follow-up.)

### 9.3 Refresh re-reads the User row

Every `/auth/refresh` re-reads the User entity row before minting the
new token. There is **no claim copy-forward** from the previous PASETO
— a row mutated since the last login (role change, `client_org_id`
reassignment, etc.) takes effect on the next refresh, not on next login.

This is load-bearing for per-record scoping: stale claims defeat the
isolation the feature exists to provide. If you need stricter
invalidation than the 1-hour token TTL, force a fresh login from the
client side (sign out + sign in).

### 9.4 `@hidden` fields are refused

A `source.user_field` may not point at a `@hidden` field on the User
schema (e.g. `password_hash`). Configuring this aborts startup with a
clear error — refusing to leak a `@hidden` value into a token, even if
the operator opted in. `@hidden` exists precisely to keep these values
off the wire; the IN-side projection respects that contract.

### 9.5 Required + null source field → 401

When a mapping declares `required = true` and its `source.user_field`
resolves to `null`/missing on the user row at login, the response is
**401** with the standard `invalid credentials` envelope (not 500).
This matches the OUT-side `required` failure mode: the contract isn't
satisfied, the user can't sign in, and the client doesn't have to
special-case a third login outcome.

### 9.6 Startup-time validation is the contract

The daemon refuses to start when any `source.user_field` declaration:

- references a field that doesn't exist on the loaded User schema
- references a `@hidden` field
- has a DSL type outside the projection vocabulary above
- pairs a User field type with an incompatible `type` (e.g. `text` and
  `long`)

There is no runtime-500 fallback. A misconfigured deployment fails fast
on boot — the error message names the offending claim and field.

### 9.7 CLI: out-of-band token issuance

For CI / operations / replay scenarios where the daemon doesn't mint
the token, `schemaforge token generate` accepts one flag per claim
type (no auto-coercion):

```sh
schemaforge token generate \
    --sub user:alice \
    --custom-claim-string client_org_id=org-42 \
    --custom-claim-long tier=2 \
    --custom-claim-bool internal=true \
    --custom-claim-set-string regions=us,eu
```

Each flag is repeatable. Type tags are explicit so a deployment
declaring `type = "string"` for `phone` doesn't silently accept
`phone=5551212` as a `long` — pick the flag that matches the declared
mapping.

### 9.8 Worked example, end to end

Same goal as §8 — per-`ClientOrg` file scoping inside a `Firm` tenant —
but operator-driven on both sides:

**User schema** (deployment override, via `@access(admin)`):

```
@access(admin)
schema User {
    email:         text(max: 512) required indexed
    display_name:  text(max: 255) required
    roles:         text[]
    role_rank:     integer required
    active:        boolean default(true)
    password_hash: text(max: 512) @hidden
    client_org_id: text(max: 64)
}
```

**`config.toml`:**

```toml
[schema_forge.authz.principal_claims.client_org_id]
type     = "string"
required = true
source   = { user_field = "client_org_id" }
```

**`policies/custom/per_org_files.cedar`** — same as §8.

**Behaviour:**

- `POST /auth/login` reads alice's `client_org_id` column → mints token
  with `custom.client_org_id = "org-42"`. ⇒ 200.
- Workspace file with `client_org == "org-42"`: `forbid` does not fire.
  ⇒ 200 (or whatever access otherwise grants).
- Workspace file with `client_org == "org-13"`: `forbid` fires. ⇒ 403.
- Bob's `client_org_id` column is `NULL`: login responds 401. The token
  is never minted; no Cedar evaluation happens; Bob fixes his account
  state out-of-band.
- Operator reassigns alice from `org-42` to `org-99`: alice's existing
  token still carries `org-42` until expiry. On her next `/auth/refresh`
  (or fresh login) the new token carries `org-99`.
