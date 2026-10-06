# OAuth login

SchemaForge can turn a GitHub, Google, or configured OIDC identity into the same
PASETO session issued by password login. Enable the `oauth` cargo feature on the
CLI or integration crate; it is off by default. For a local PostgreSQL build:

```sh
cargo run -p schema-forge-cli --no-default-features --features postgres,oauth -- serve
```

Provider credentials use acton-service's existing configuration. Set each
provider's redirect URI to the forge callback, then configure the account policy
and frontend return destinations:

```toml
[auth.oauth.providers.github]
client_id = "github-client-id"
client_secret = "load-from-your-secret-store"
redirect_uri = "https://forge.example.com/api/v1/forge/auth/oauth/github/callback"
scopes = ["user:email"]

[schema_forge.auth.oauth]
enabled = true
signup = "invite_only" # or "open"
default_roles = ["member"]
return_to_allowlist = ["https://console.example.com/"]
password_login = true
```

Use canonical acton-service environment names to supply secrets, for example
`ACTON_AUTH__OAUTH__PROVIDERS__GITHUB__CLIENT_SECRET`. Keep provider secrets out of
committed TOML. Google uses the `google` provider key. Other provider names use
OIDC and require `authorization_endpoint`, `token_endpoint`, and
`userinfo_endpoint` in addition to the credentials and callback URI.

Enabled configuration must have a nonempty valid return allowlist. Return targets
are checked by parsed origin, port, and path boundary. `/app` permits `/app` and
`/app/...`, but not `/application`. Userinfo, fragments, and ambiguous encoded
path separators are rejected. HTTPS is required except HTTP on localhost or
loopback for development. Existing application query parameters are preserved.
The upstream `auth.oauth.enabled` setting does not expose SchemaForge routes;
`schema_forge.auth.oauth.enabled` controls them.

## Redirect and exchange

The four OAuth routes are public and require no existing bearer:

| Method | Route under `/api/v1/forge` | Result |
| --- | --- | --- |
| GET | `/auth/oauth/providers` | Sorted JSON array of configured names |
| GET | `/auth/oauth/{provider}/start?return_to=...&invite_id=...` | 302 to the provider; invite is optional |
| GET | `/auth/oauth/{provider}/callback?code=...&state=...` | 302 to the allowlisted frontend with a one-time `code` |
| POST | `/auth/oauth/exchange` | `{ "token": "...", "expires_at": "...", "roles": [...] }` |

Start state is opaque, single use, and valid for ten minutes. It binds the
provider, return URL, and invite. The callback consumes it before contacting the
provider. Unknown providers return 404. Invalid return URLs return 400. Missing,
expired, reused, or mismatched state returns 401. A provider must supply a present,
valid, verified email; otherwise the callback returns 401.

The frontend receives an opaque login code, never a PASETO in its URL. That code
is valid for **60 seconds** and can be exchanged **once**. Redirects and exchange
responses prohibit caching. Provider tokens are not persisted. Exchange also
refuses an account disabled or deleted after the callback.

```js
const url = new URL(window.location.href);
const code = url.searchParams.get("code");
if (code) {
  url.searchParams.delete("code");
  history.replaceState(null, "", url);
  const response = await fetch("https://forge.example.com/api/v1/forge/auth/oauth/exchange", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code }),
  });
  if (!response.ok) throw new Error("Sign-in expired. Start again.");
  const session = await response.json();
  // Keep session.token in your application's protected session storage and
  // send it in the Authorization: Bearer header on subsequent API requests.
}
```

Both state stores are bounded, process-local acton-service state managers. A
restart invalidates pending flows. Route the entire start, callback, and exchange
sequence to the same process when using multiple instances.

## Accounts, invitations, and authorization

A provider/subject pair resolves its durable `OAuthIdentity` link to a `User`.
Provider subjects are opaque and case sensitive. Email is required for onboarding,
but is never used to attach a new provider to an existing account. A matching
email without a link returns 409; an administrator must arrange that link.
Existing links continue to resolve if a provider's verified email changes.

For open signup, new accounts receive `default_roles`. Invitation-only signup
without an invite returns 403. An invite must still be pending and unexpired,
and its stored PASETO is cryptographically reverified. Its signed email must
match the provider's verified email. Signed role and tenant claims determine the
grants, including when mirrored database columns differ. Consumption follows
successful account, membership, and identity writes, as in password invite
acceptance. These onboarding writes share that flow's non-atomic ordering; a
storage failure can require administrator cleanup before retrying.

New OAuth accounts have no `password_hash`. The existing system field is optional
and hidden; password validation returns normal bad credentials (401). The
existing Cedar-authorized `/users/{username}/password` endpoint can set the first
password. `password_login = false` makes `/auth/login` return 404 while OAuth is
enabled. The generated site's login page continues to use password login.

`OAuthIdentity` is a shared system schema, outside tenant scoping. Its hidden
computed `identity_key` encodes the provider/subject pair injectively and carries
a table-wide unique constraint on both backends. Only platform administrators
can create, update, or delete links through entity routes, even when a custom
policy otherwise permits those actions. `GET /auth/me` reports
`identities: [{ "provider": "github", "subject": "123" }]`.

After identity resolution, OAuth uses the shared password-login tail: current
roles, memberships and tenant requirements, user-field principal claims, token
creation, `last_login`, and audit. A membership-required deployment refuses a
user without membership. `/auth/refresh` rebuilds grants normally; it does not
contact the provider. Configured providers use the audited upstream registry;
login audit metadata records `source: "oauth:<provider>"`.

Startup seeds the new system schema. Existing User rows and password hashes need
no migration. Builds without the feature reject enabled OAuth configuration;
disabled OAuth routes are absent. OpenAPI export includes the four routes when
the exporter is built with `oauth`, and exchange reuses `LoginResponse`.
