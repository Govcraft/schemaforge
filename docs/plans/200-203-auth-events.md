# Issues 200-203 implementation plan

Complete OAuth login and authenticated entity event streams, fix multiline hook
intent comments, and validate the hook scaffold dependency pin. Deliver one PR
that closes all four issues, then publish the next minor CLI release with verified
release assets. Use signed Conventional Commits for small completed increments.
Local cargo checks, nextest filters, and Clippy package/feature selections cover
only the current change. The aggregate PR runs the full CI matrix.

## Hooks, issues 201 and 202

Trim outer blank lines and prefix every intent line as a Rust doc comment in
`commands/hooks.rs::render_impl_stub`. Exercise multiline before/after handlers
through `tests/hooks_generate.rs` and compile one generated fixture manually.
The 0.45.0 pin fix has merged in PR 205. Strengthen its manifest test to compare
with the runtime crate as well as the CLI, so future drift fails validation.

## OAuth, issue 200

Add opt-in `oauth` features to the integration crate and CLI, preserving disabled
defaults. Define configuration under `schema_forge.auth.oauth`: enabled,
SignupPolicy (open/invite_only), default roles, return URL allowlist, and password
login selection. Validate configuration before exposing routes. Provider config
remains the framework's `auth.oauth` section. Use OAuthProviderRegistry's audited
constructor and MemoryOAuthStateManager through an injectable runtime context.
External provider subjects and opaque upstream state/code values retain their
upstream meanings; use wrappers for provider identity and validated return URLs.
Generated persistent account/link identifiers reuse existing typed EntityId.

Implement start, callback, exchange, and providers handlers under `/auth/oauth`.
`validate_return_to(config, candidate)` must compare parsed URL origins and allowed
path prefixes rather than unsafe string prefixes. State binds provider, validated
return target, invite, and flow kind. Callback consumes state once, requires verified
email, and resolves a durable identity link. Never implicitly link a provider to
an existing password account merely because its email matches. Invite claims are
reverified and decide grants; open signup grants only configured default roles.
Handle concurrent signup/link collisions with typed backend uniqueness errors.

Add OAuthIdentity as a system schema after User, enforcing provider/subject pair
uniqueness on both backends and limiting direct mutation to platform admins.
EntityAuthStore and object-safe adapters gain identity lookup/link/list and
passwordless account creation with compatible defaults for other auth stores.
User.password_hash is already optional; preserve hidden projection and add actual
passwordless credential/password-setting regressions.

Extract a shared `complete_login(context, user, source)` tail for password and
OAuth login: live membership policy, principal claims, token generation,
record_login, and success/failure audit. Callback stores a 60-second single-use
login-code payload and redirects with only that opaque code. Exchange returns
LoginResponse. `/auth/me` reports linked provider/subject identities. The upstream bearer middleware matches prefixes, so whitelist only the bounded
`/api/v1/forge/auth/oauth/` namespace. Tenant exemptions recognize exact endpoint
shapes and validated provider segments.

Test handlers with an injected mock provider and real account storage: redirects,
provider selection, verified/missing email, state/code reuse and expiry, allowlist
bypasses, invite claims/consumption, signup policies, password-only disablement,
passwordless login refusal, tenant membership, principal projection, audit, and
provider identity uniqueness on PostgreSQL and SurrealDB. Add OpenAPI routes and
`docs/oauth-login.md` linked from the skill.

## Entity streams, issue 203

Add opt-in `sse` features to the integration crate and CLI, disabled by default.
EventsConfig supplies enabled, keep-alive interval, bounded capacity, per-user
connection limit, and reconnect delay, with validated positive bounds. Add a
process-owned broadcaster/runtime extension and a conditional
`GET /schemas/{schema}/events` route. Authenticate bearer claims and apply the
same active-tenant middleware as entity reads. Reject unknown query fields through
the existing query parser. No bearer or token data belongs in a query string.

Use typed subscription/connection handles and an RAII connection lease for limits
and cleanup. `project_event_for_subscriber(context, snapshot)` must reuse the read
policy and field projection against the full committed record before serializing.
Extract shared read authorization/projection helpers where needed. Match equality
filters only on fields the subscriber is permitted to inspect, preventing hidden
field probing. Use pre-delete records to authorize id-only delete events.

Publish one committed mutation snapshot from the common create/PUT/PATCH/delete
notification point, sharing WebhookEvent metadata and preserving hook/webhook
behavior. Ensure per-entity commit ordering; inspect actor commit serialization
before choosing the final enqueue point. Bounded nonblocking delivery must drop
and disconnect a lagging subscriber without delaying or failing the writer.
Recheck membership and close affected connections with a closed event on membership
or hierarchy changes. Reevaluate policy for each event and handle expired identity
credentials. No replay: ignore Last-Event-ID and document refetch on reconnect.

Test stream frames and GET-equivalent payloads for create/update/delete, hidden
and restricted fields, Cedar/tenant refusal, filter validation, disabled/feature-off
404s, anonymous 401, unknown schema, capacity disconnection, keep-alive timing,
connection limits/cleanup, membership closure, ordering, and backend-neutral
mutation integration. Add text/event-stream OpenAPI responses and docs/events.md
with a fetch-based browser reader.

## CI, versions, and release

Keep new features off by default, but add explicit OAuth/SSE coverage for both
backends to the aggregate CI and supported binary builds. Preserve existing
feature-off validation. Select package minor bumps after public API/storage
changes are finalized; the CLI release will be at least 0.48.0 because this adds
functionality beyond the prepared 0.47.0. Update dependents through cargo add for
dependency requirement changes, update Cargo.lock, changelog, and migration docs.

Before the single PR, inspect all changes and run focused remaining regressions.
The PR closes #200, #201, #202, and #203. After required CI passes, merge through
protected main, verify issue closure, create the established signed vX.Y.Z tag,
and publish using the release workflow. Verify the published release, all intended
archives/signatures/manifests, and smoke-test downloaded Linux binaries with
feature/config behavior. Complete the goal only when every issue is closed and
the new release is published successfully.

## Completed increments and focused validation

- #201: multiline stubs preserve paragraphs as doc comments. All 17 hook
  generation integration tests and targeted CLI Clippy passed; a fresh generated
  multiline hook project compiled against acton-service 0.45.0.
- #202: the merged upgrade repaired the scaffold pin. A manifest regression now
  compares the scaffold, CLI, and runtime pins.
- #200 storage: validated provider/subject types, passwordless creation, exact
  link lookup, identity listing, and unique system-schema storage are committed.
  All 24 account-store tests passed. PostgreSQL and SurrealDB both returned typed
  UniqueViolation for a second user claiming the same provider/subject pair.
- The real invite test exposed SurrealDB's timestamp-string heuristic. Native
  datetime conversion now preserves typed datetimes while RFC3339 text remains
  text, with a focused regression.
- #200 HTTP: 24 selected OAuth, password, and identity-storage tests passed,
  including custom broad-permit resistance, signed invite tampering, default
  grants, required membership, user-field claims, current-grant refresh,
  passwordless refusal, disabled routes, state/code expiry and reuse. The CLI
  OpenAPI integration test and focused runtime/CLI Clippy passed.
- PostgreSQL and SurrealDB framework features are mutually exclusive. Keep their
  CI builds separate; the password-login HTTP tests now run with either backend
  selection because their real in-memory storage fixture is a dev dependency.
