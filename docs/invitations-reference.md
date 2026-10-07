# User invitations and onboarding reference

Invite a person into a SchemaForge deployment and provision their account and tenant membership when they accept. They can use a password or a configured OAuth provider. Operators can deliver the link through SMTP or share it themselves.

## Issuing an invitation

`POST /api/v1/forge/auth/invites` requires a bearer token and explicit `Action::"InviteUser"` permission.

```json
{
  "email": "newuser@agency.gov",
  "display_name": "New User",
  "tenant_type": "Organization",
  "tenant_id": "entity_...",
  "role": "member"
}
```

`email` is required and becomes the login identifier. `display_name` and `role` are optional. The role is stored on both the future User and TenantMembership. A tenant target requires both `tenant_type` and `tenant_id`; the type must be a configured tenant schema. Non-platform callers must have the exact target in their effective tenant chain, including any active-tenant narrowing. Invalid or incomplete targets return 422; targets outside the caller's chain return 403.

Success returns `201 Created`:

```json
{
  "invite_id": "opaque-reference",
  "email": "newuser@agency.gov",
  "expires_at": "2026-10-13T18:22:11Z",
  "delivery": "link",
  "accept_url": "https://app.agency.gov/invite/accept?invite=opaque-reference"
}
```

`delivery` is `smtp` when the relay accepted the email, or `link` when the caller should share `accept_url`. The opaque reference is the PASETO token id (`jti`). The full token stays on the server. The invitation expires after seven days.

| Status | Cause |
|---|---|
| 401 | Missing or invalid bearer token. |
| 403 | Missing InviteUser permission, unauthorized tenant, upward role grant, or unauthorized platform_admin grant. |
| 422 | Invalid email or tenant target, or an account already exists for the address. |
| 502 with `invite_delivery_failed` | The invitation was stored, but SMTP delivery failed. |

SMTP delivery failures have a recoverable response:

```json
{
  "error": "invite_delivery_failed",
  "message": "Invitation created, but email delivery failed. Share the accept link to complete onboarding.",
  "invite_id": "opaque-reference",
  "accept_url": "https://app.agency.gov/invite/accept?invite=opaque-reference",
  "delivery": "failed"
}
```

The invitation remains Pending and its link is valid. Share that link through another channel. Creating a second invitation is unnecessary. SMTP diagnostics are stored in the metadata of the `forge.invite.send_failed` audit event and are excluded from the response.

## Authorization and policy migration

Inviting users and directly creating accounts are separate capabilities. `CreateUser`, including permission derived from User's `@access(write: [...])`, no longer authorizes invitations. Existing custom invitation policies must explicitly grant `InviteUser` after upgrading. Platform administrators retain their global permit. Granting `InviteUser` alone does not allow `POST /users`.

A tenant owner can receive invitation permission without general User administration:

```cedar
permit (
    principal in Forge::Group::"owner",
    action == Action::"InviteUser",
    resource is User
) when {
    resource has "_tenant" && principal in resource["_tenant"]
};
```

`GET /api/v1/forge/users/roles` keeps the full catalog for callers with ListUser permission. Invitation-only callers receive only roles that their InviteUser policy permits in their active tenant, including the role-rank guard. This role picker does not grant User listing or creation.

Declare `owner` and invited roles in `policies/role_ranks.toml`. The resource is a proposed User with the invitee's computed `role_rank` and target `_tenant` reference, so custom policies can inspect the granted rank and tenant. The endpoint evaluates that concrete resource directly, allowing policies scoped to the proposed target. A placeholder preflight is unnecessary.

The global User-management rank forbid includes `InviteUser`: a caller cannot invite a role above their rank even if another policy grants Invite. Only an existing platform administrator can grant `platform_admin`. Configured-tenant and caller-chain validation happens before token minting or persistence.

## Listing and revoking invitations

`GET /api/v1/forge/auth/invites` requires a bearer token and explicit
`Action::"ListInvites"` permission. It returns unexpired Pending invitations
in the caller's active tenant. Use `X-Active-Tenant: Organization:entity_...`
to select a membership when the caller belongs to several tenants.

```json
{
  "invitations": [
    {
      "id": "forgeinvitation_...",
      "email": "newuser@agency.gov",
      "role": "member",
      "inviter": "user:owner@agency.gov",
      "created_at": "2026-10-07T18:22:11.123Z",
      "expires_at": "2026-10-14T18:22:11Z"
    }
  ],
  "next_offset": null
}
```

`created_at` is the UTC time encoded in the invitation's UUIDv7 TypeID, with
millisecond precision. It records when the identifier was generated and works
for existing invitations without a database migration. A legacy identifier
using another UUID version returns `null`. The response never includes the
stored token, its opaque acceptance reference, or an acceptance URL.

Pagination defaults to `limit=50`; allowed limits are 1 through 100. Follow
`next_offset` with the same tenant and limit until it is `null`. Offsets may
not exceed 1,000,000. Unknown or invalid query parameters return 400. Expired,
consumed, revoked, and malformed invitations are omitted.

`DELETE /api/v1/forge/auth/invites/{id}` requires explicit
`Action::"RevokeInvite"` permission. Use the row `id` from the listing, rather
than the invitation's acceptance reference. Success returns `204 No Content`.
Repeating revocation returns 204; an already consumed invitation returns 409.
Unknown invitations and invitations outside a tenant caller's active tenant
return 404. Revocation emits a `forge.invite.revoked` audit event containing
the row identifier, without invitation credentials.

Grant these permissions separately from issuing invitations or administering
Users. For example:

```cedar
permit (
    principal in Forge::Group::"owner",
    action in [Action::"ListInvites", Action::"RevokeInvite"],
    resource is User
) when {
    resource has "_tenant" && principal in resource["_tenant"]
};
```

Listing checks `ListInvites` against a proposed User with an empty email, no
roles, and the selected `_tenant`, then evaluates each pending invitation as
its proposed User with the invited role. Grant the listing scope as in the
example above; a policy that requires a specific invitation id or email cannot
provide this initial listing permission. Per-invitation custom denials omit that row. Revocation checks the concrete
invitation. Listing and revoking do not grant roles, so the invitation creation
rank guard does not apply. An owner with explicit permission can revoke an
administrator's pending invitation in their own tenant.

Platform administrators retain the global Cedar permit and may act across
tenants. Without an active-tenant header, their listing includes all tenants;
supplying a valid configured tenant narrows it. Tenant callers cannot widen
their listing or revoke another tenant's invitation through a custom policy.

`ListInvites` is reserved for private invitation management. If an application
schema is named `Invites`, its entity listing action is `Action::"List:Invites"`;
its read action remains `Action::"ReadInvites"`. Generated `@access` and tenant
policies use the disambiguated action automatically. Update custom policies
that list application `Invites` entities to use `List:Invites` when upgrading.
Application entity read/list permissions do not grant private invitation access.

Acceptance and revocation compete through an atomic Pending-state transition.
If revocation succeeds, password and OAuth acceptance refuse the invitation.
If acceptance has already claimed it, revocation returns 409.

## Delivery configuration

SMTP remains the default delivery mode. `enabled` controls SMTP only; selecting `link` bypasses SMTP whether `enabled` is true or false. A link deployment requires no mail host, sender address, credentials, or SMTP service:

```toml
[schema_forge.email]
delivery = "link"
enabled = false
public_base_url = "https://app.agency.gov"
```

In link mode, `public_base_url` must be an absolute HTTP(S) URL with a host and no credentials, query, or fragment. Configuration validation rejects missing or malformed values before issuing an invitation.

For SMTP delivery:

```toml
[schema_forge.email]
delivery = "smtp"          # default
enabled = true
host = "mail.agency.gov"
port = 465                 # implicit TLS default
tls = "implicit"           # or "start_tls", usually port 587
from = "noreply@agency.gov"
username = "noreply@agency.gov"
public_base_url = "https://app.agency.gov"
```

`host` and `from` are required when SMTP is enabled. Credentials are optional for unauthenticated relays. Supply the password through `SCHEMAFORGE_SMTP_PASSWORD`; keep it out of committed configuration. This dedicated variable supports the existing `SCHEMAFORGE_*` convention because the framework's underscore-split `ACTON_` environment mapping cannot address `schema_forge` reliably.

SMTP with `enabled = false` still stores the invitation, then returns the typed delivery failure with its accept link. SMTP retains the site-relative link fallback when `public_base_url` is absent; configure an absolute URL for links sent outside the application.

## Accepting an invitation

`POST /api/v1/forge/auth/invites/accept` is public. Possession of the unexpired, unused invitation reference authorizes acceptance:

```json
{
  "invite_id": "opaque-reference",
  "password": "the-invitee-chosen-password",
  "display_name": "Optional Override"
}
```

Password validation matches the account-creation policy. The server finds the Pending row, verifies its stored PASETO, and atomically claims it before using the signed role and tenant claims to create the User and optional TenantMembership. Account and membership writes remain separate operations. A provisioning failure leaves the invitation consumed to prevent reuse; partial acceptance failures require operator recovery. An existing account returns 409; expired, consumed, or revoked links return the same 422 refusal.

Success returns 201 with the created email and roles:

```json
{ "email": "newuser@agency.gov", "roles": ["member"] }
```

## Branding and security properties

Invitation emails reflect the deployment's sign-in methods. Password deployments
ask the invitee to choose a password. OAuth-only deployments name the configured
providers and ask the invitee to sign in using the invited email. Deployments
with both methods explain either option. The frontend invitation page should
pass the opaque reference as `invite_id` when starting OAuth, alongside its
allowlisted `return_to`. See [OAuth login](oauth-login.md) for that flow.

`[schema_forge] project_name` brands invitation email subjects, bodies, and a bare `from` address's display name. An explicit mailbox display name overrides that default. `schemaforge init` seeds this value from the project name.

Invitations live in the internal ForgeInvitation store, outside the public schema registry and entity CRUD routes. Signed claims are authoritative over stored mirror columns. References expire after seven days and are consumed on acceptance. Treat returned links as credentials and share them only with the intended recipient.

Audit events include `forge.invite.created`, `forge.invite.accepted`, `forge.invite.rejected`, `forge.invite.revoked`, `forge.invite.send_failed`, and `forge.access.denied`.
