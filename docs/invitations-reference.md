# User invitations & onboarding reference

Invite a person into a SchemaForge deployment by email, let them set their own password, and provision their account and tenant membership on acceptance. This document covers the two HTTP endpoints, the authorization model, the SMTP/email configuration (including how the password is supplied without ever touching git), and the `project_name` branding that controls what the invitee sees. Three readers — operators wiring SMTP for a deployment, integrators calling the invite endpoints from a console or script, and security auditors tracing the trust boundaries — should jump by heading. Scope is configuration, the wire contract, and security properties.

## At a glance

| | |
|---|---|
| Issue an invite | `POST /api/v1/forge/auth/invites` — **authenticated** |
| Accept an invite | `POST /api/v1/forge/auth/invites/accept` — **public** |
| Token | PASETO v4.local, `purpose = "invite"`, 7-day expiry |
| Email | `[schema_forge.email]`, disabled by default; SMTP password via env only |
| Branding | `[schema_forge] project_name` |
| Audit events | `forge.invite.created`, `forge.invite.accepted`, `forge.invite.rejected`, `forge.invite.send_failed`, `forge.access.denied` |

The account is created **on acceptance**, not at invite time — see [Why create the account at accept](#why-create-the-account-at-accept). The invitation row is the pending state; no half-provisioned, password-less account ever sits in the user table.

## Issuing an invitation

`POST /api/v1/forge/auth/invites` — requires a valid bearer token.

Request:

```json
{
  "email": "newuser@agency.gov",
  "display_name": "New User",
  "tenant_type": "Organization",
  "tenant_id": "01H...",
  "role": "member"
}
```

| Field | Required | Meaning |
|---|---|---|
| `email` | yes | Invitee address; becomes `User.email`, the login identifier. Structurally validated (one `@`, a dotted domain, no whitespace, ≤ 512 chars). |
| `display_name` | no | Seeded onto the future account. |
| `tenant_type` | no | Tenant **root** type the invitee joins (e.g. `"Organization"`). Tenancy is polymorphic — this is whatever schema is annotated `@tenant(root)`. |
| `tenant_id` | no | Tenant root entity id. |
| `role` | no | Role granted to the invitee — used as **both** their `User` role and their `TenantMembership` role. |

Success — `201 Created`:

```json
{
  "invite_id": "Hk9c...opaque...",
  "email": "newuser@agency.gov",
  "expires_at": "2026-06-04T18:22:11Z"
}
```

`invite_id` is the opaque, high-entropy reference that is emailed to the invitee. It is the PASETO's token id (`jti`); the full token is **never** placed in the email — only this reference is.

Failure modes:

| Status | Cause |
|---|---|
| `401` | No / invalid bearer token. |
| `403` | The caller lacks `Create` on the user schema, or the requested `role` would exceed the caller's own role rank (see [Authorization](#authorization)). Emits `forge.access.denied`. |
| `422` | `email` failed validation, or an account already exists for that address. |
| `5xx` | The invite row was written but **email delivery failed**. The row stays `Pending` and can be re-sent once SMTP is healthy; emits `forge.invite.send_failed`. |

The order is deliberate: the invitation is persisted **before** the email is sent, so the accept link always resolves to a row. Delivery failure is surfaced (fail-closed) rather than swallowed.

## Accepting an invitation

`POST /api/v1/forge/auth/invites/accept` — **public** (no bearer token; the invite token *is* the credential).

Request:

```json
{
  "invite_id": "Hk9c...opaque...",
  "password": "the-invitee-chosen-password",
  "display_name": "Optional Override"
}
```

`password` is validated against the same policy as `POST /users`. `display_name` falls back to the invite's value, then to the email.

Success — `201 Created`:

```json
{
  "email": "newuser@agency.gov",
  "roles": ["member"]
}
```

What happens, in order:

1. Look the row up by `invite_id`. If it is not `Pending` or has expired → `422`, emits `forge.invite.rejected`.
2. Reconstruct the full PASETO from the stored `token` column and re-verify it cryptographically. **The signed claims are authoritative** over the database columns (defense-in-depth against DB tampering): role and tenant are read from the verified token, not the row.
3. Refuse if an account now exists for the address (`409 Conflict`, `user_exists`).
4. Create the `User`, then add the `TenantMembership` (if a tenant was scoped), then mark the invitation `Consumed` — **last**, so a partial failure leaves the link retryable.

A replayed link (already `Consumed`) is rejected with `422`.

## Authorization

The privilege checks run **at invite time**, against the proposed role, so deferring account creation to accept does not weaken authorization. An invite can never confer access the inviter could not grant directly. Three guards, mirroring `POST /users` exactly:

1. **Schema access** — `check_schema_access(Create)` on the user schema.
2. **Role-grant guard** — `caller_can_grant_roles`: only a `platform_admin` may grant `platform_admin`.
3. **Cedar rank guard** — `authorize(Create, User, <synthetic user carrying the proposed role>)` against `role_ranks.toml`; inviting a role above the caller's own rank is denied.

## Email configuration

`[schema_forge.email]` — **disabled by default**. When disabled, the invite endpoints fail closed with a clear "email not configured" error rather than silently dropping mail.

```toml
[schema_forge.email]
enabled = true
host = "mail.agency.gov"
port = 465                 # 465 = implicit TLS (default); 587 = STARTTLS
tls = "implicit"           # or "start_tls"
from = "noreply@agency.gov"
username = "noreply@agency.gov"
public_base_url = "https://app.agency.gov"
```

| Field | Default | Notes |
|---|---|---|
| `enabled` | `false` | Master switch. |
| `host` | — | SMTP relay hostname. Required when enabled. |
| `port` | `465` | |
| `tls` | `implicit` | `implicit` (SMTPS, port 465) or `start_tls` (port 587). |
| `from` | — | `From` mailbox. Required when enabled. A bare address is branded with `project_name` (see below); embed a display name here — `"Agency <noreply@agency.gov>"` — to override. |
| `username` | — | SMTP AUTH user. Omit for an unauthenticated relay. |
| `password` | — | **Never** written here — see below. |
| `public_base_url` | — | Used to build the absolute accept link, `{public_base_url}/invite/accept?invite={invite_id}`. Without it the link is site-relative. |

TLS uses the workspace `aws-lc-rs` rustls provider (FIPS-aligned, no `ring`).

### SMTP password — environment only

The SMTP password is **never** read from `config.toml` and must never be committed. Supply it at runtime through the `SCHEMAFORGE_SMTP_PASSWORD` environment variable:

```sh
SCHEMAFORGE_SMTP_PASSWORD="$(rbw get 'smtp-relay')" schemaforge serve
```

acton-service's `ACTON_`-prefixed Figment env layering cannot address the `[schema_forge]` section (its `Env::split("_")` shatters the underscore in the section key), so this dedicated variable — matching the existing `SCHEMAFORGE_*` convention used for the token key and trust policy — is the supported path. `serve` reads it and fills `EmailConfig.password` before constructing the transport.

## Branding: `project_name`

```toml
[schema_forge]
project_name = "Bob's Dog Scheduling"
```

`project_name` (default `"SchemaForge"`) is the human-facing name of the deployment. An onboarding user should see the application they are joining, not the engine. It drives:

- **The invitation email body** — "You have been invited to join *Bob's Dog Scheduling*."
- **The invitation email subject** — "You've been invited to *Bob's Dog Scheduling*".
- **The `From` display-name**, when `from` is a bare address — recipients see `Bob's Dog Scheduling <noreply@…>`. An explicit display name in `from` is respected verbatim, so operators keep exact control for deliverability.

`schemaforge init <name>` seeds `project_name` from the project name into the generated `config.toml`, so the name carries from scaffold into runtime. Edit it any time.

## Security properties

- **Store, not schema.** Invitations live in an internal `ForgeInvitation` table that is provisioned at boot but **never inserted into the `SchemaRegistry`**. Because `/schemas` and every entity route resolve through that registry, the table — and the token material it holds — is unreachable through the public entity API.
- **Single-use, expiring.** 7-day TTL on both the PASETO `exp` and the stored `expires_at`; consumption flips the status so a replayed link is rejected.
- **Signed claims authoritative.** On accept, role and tenant come from the cryptographically verified token, not from mutable DB columns.
- **Fail-closed delivery.** A send failure is a `5xx` with the invite left `Pending`; it never reports success on undelivered mail.
- **No secret on disk.** The SMTP password enters only through the environment.
