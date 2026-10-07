# Changelog

All notable changes to SchemaForge are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project
is pre-1.0; breaking changes bump the **minor** version per
[SemVer](https://semver.org/#spec-item-4) for the `0.y.z` series.

## [Unreleased]

## [0.49.1] - 2026-10-06

### Changed

- Main pushes reuse successful PR CI when the exact file tree and required
  component coverage match verified validation evidence. Missing evidence runs
  normal component validation; nightly, manual, and release runs validate afresh.
  Portable component tests, clippy, and doctests appear as separate CI steps.
- Pull requests select affected component checks. Version-only release preparation
  uses metadata validation; nightly and release tags run full validation.
  Release packaging waits for tests of the tagged commit. CI installs prebuilt
  nextest and separates PostgreSQL from SurrealDB checks.
- Site generation and generated-file primitives live in `schema-forge-codegen`,
  with site branding types in `schema-forge-config`. The CLI supports a tooling-only
  `--no-default-features` build; backend builds retain the complete executable.
- Portable runtime tests no longer compile SurrealDB. Real database integration
  tests are enabled explicitly with `test-surrealdb`. Generator-only browser smoke
  tests use a pinned validated server; full validation tests the candidate server.

- Build the server, backends, CLI, and generated hook services on acton-service
  0.46.0. Audit records reach ordinary logs (console, journald, and OTLP log
  exporters) only with `[audit] otlp_logs_enabled = true`, which defaults to
  `false`. Before, every audit event was logged at `info` with its subject and
  client IP whatever the setting. Storage and syslog export are unchanged, and
  so are the Cedar decision lines under the `schema_forge_acton::authz` tracing
  target. The privacy fix is upstream: reported in Govcraft/acton-service#172
  and fixed in Govcraft/acton-service#174.
- `hooks generate` scaffolds load `Config::<()>::load()`. acton-service 0.46.0
  made `GrpcServicesBuilder::build` generic over the state type, so a bare
  `Config::load()` no longer compiles (E0283). Existing hook services moving
  their `acton-service` pin to 0.46.0 to match the forge need the same one-line
  change in `src/main.rs`; additive regeneration leaves that line alone, and
  `--regenerate` rewrites the whole file.

### Fixed

- Required hook failures return a fixed client-safe message with HTTP 503 and
  `hook_unavailable`. Hook endpoints, timeout values, and transport or protocol
  diagnostics remain in server logs.
- `hooks generate` preserves existing service constructors and dependency wiring
  when adding hooks for another schema. `--check` detects pending changes to the
  scaffolded registration regions and respects explicit rewrite flags.
- `schemaforge policies validate` now checks custom policies against the
  built-in system schemas (`User`, `TenantMembership`, `OAuthIdentity`,
  `WebhookSubscription`), as `serve` does. Policies that reference them,
  including the tenant-owner `InviteUser` policy in the invitations reference,
  no longer fail offline with `unrecognized action` or `unrecognized entity
  type` while compiling in the daemon. A project schema that redefines a system
  schema replaces the built-in one, as at startup. JSON and plain output keep
  their fields, and `schema_count` still counts only the project's schemas;
  the human summary also reports the system schemas.
- Entity event streams accept password and OAuth login tokens. Since 0.48.0 the
  live identity check looked accounts up by the token subject (`user:<username>`)
  rather than the stored username, so every tenant member was refused with 403
  `Stream authorization is no longer valid.`, and streams of accounts without a
  tenant chain stayed open after deactivation or role changes. Platform
  administrators who hold tenant memberships are now checked without tenant
  scope, matching the tenant-scope middleware. Streams still close and stay
  refused after deactivation, role changes, or membership removal.

## [0.49.0] - 2026-10-06

### Added

- Optional `[schema_forge.tenancy] creator_role` commits a tenant root and its
  creator's membership atomically on PostgreSQL, SurrealDB, and SQL Server.
  PostgreSQL create receipts include the membership in the same commit and
  preserve it when reconciling a request.
- Scoped membership roles enter signed login and refresh claims and apply only
  within the selected tenant. Creator grants do not change global account roles.
  Entity streams recheck scoped roles and close when access is revoked.
- Independent Cedar `InviteUser` permission lets tenant owners invite without
  direct user creation or listing permission. The existing role-rank and tenant
  delegation restrictions still apply. The generated invitation form supports
  these owners and manual sharing of acceptance links.
- `[schema_forge.email] delivery = "link"` returns an acceptance URL without
  SMTP. SMTP failures after persistence return 502 `invite_delivery_failed`
  with `invite_id` and `accept_url`, so clients can recover the existing invite.
- Configured `schema_forge.tenancy.default_tenant` assigns new open OAuth
  signups to an existing tenant root before login completes. Signed invitations
  retain their configured tenant rather than using this default.

### Changed

- Invitations require an explicit `InviteUser` policy. Existing non-platform
  inviters granted only `CreateUser` must add this permission; platform
  administrators retain their existing access.
- Startup rejects enabled open OAuth signup with tenancy unless a valid,
  existing default tenant is configured. `creator_role` alone cannot provide
  the membership needed for a first login. See the tenancy and invitations
  references for configuration and migration examples.

## [0.48.0] - 2026-10-05

### Added

- Opt-in authenticated entity SSE streams with canonical GET projection, Cedar
  and tenant authorization, equality filters, bounded nonblocking delivery,
  per-user connection limits, keep-alives, live membership revocation, and
  process-local commit ordering. Supported release binaries include OAuth and
  SSE support; both remain disabled until configured.
- Opt-in OAuth login with audited GitHub, Google, and custom OIDC providers,
  verified email, signed invitations, durable unique identity links, and a
  single-use 60-second frontend exchange code. OAuth-created accounts leave
  the existing optional, hidden `password_hash` null and can set a first
  password through the existing authorized endpoint.
- Shared login completion preserves tenant membership requirements, principal
  claim projection, login timestamps, and PASETO refresh behavior across password
  and OAuth sign-in. `/auth/me` lists linked provider identities.

### Fixed

- Render every line of multiline hook intent as a Rust doc comment, preserving
  paragraphs and trimming outer blank lines so generated handlers compile.
- Verify that generated hook services, the CLI, and the runtime share the same
  acton-service dependency version.

- Upgrade acton-service to 0.45.0 across the runtime, backends, CLI, and generated
  hook services. Preserve configured listener binds supplied with canonical
  `ACTON_SERVICE__BIND` environment variables.
- Adopt upstream configuration validation: framework tables reject unknown keys;
  canonical environment names use `__` between table segments. Unambiguous
  legacy names remain supported, while ambiguous or unresolved compound names
  produce migration errors.
- Health and readiness probes bypass rate limiting by default. Anonymous quotas
  can be configured separately, and governor reports actual remaining capacity
  with `Retry-After` on rate-limit responses instead of `X-RateLimit-Reset`.

### Migration

- Existing password accounts keep their hashes. Startup seeds the shared
  `OAuthIdentity` system schema; provision it through the normal schema migration
  path before enabling OAuth on an existing deployment. OAuth account/invite
  changes span multiple storage operations; provider/subject uniqueness prevents
  duplicate linking, while an interrupted signup can require administrator repair.
- Library users should update acton to 0.46.0, backend to 0.20.0, PostgreSQL and
  SurrealDB adapters to 0.15.0, and MSSQL to 0.7.0. Core remains compatible at
  0.19.2. Align directly imported acton-service `Claims` with 0.45.0 and update
  Rust `SchemaForgeSettings` and `MeResponse` struct literals for their new fields.
- OAuth and events remain disabled until configured. Release binaries include
  both features. SSE is process-local, has no replay/outbox, and requires clients
  to refetch on reconnect. Use canonical `ACTON_SERVICE__...` environment names.

## [0.46.1] - 2026-09-25

This release includes the changes prepared for v0.46.0. The v0.46.0 source tag
is retained, but its binary release was not published.

### Runtime and API behavior

- Apply target schema, record, and field authorization consistently when resolving
  related display labels and derived collection IDs, including export labels.
- Validate tenant declarations consistently across startup, CLI schema application,
  and runtime schema changes. Applications with a tenant root must annotate every
  application schema; built-in system schemas remain shared.
- Validate relation targets against tenant scope and read authorization before
  create, PUT, or PATCH persists. Platform administrators retain their documented
  cross-tenant capabilities.
- Apply hidden-field projection to relation display labels and webhook payloads.
  Webhooks use a fixed conservative field policy and plain JSON payload version 2.
- Enforce configured webhook URL schemes and public destinations during
  configuration and delivery. Delivery checks and pins DNS results, with redirects
  and environment proxies disabled.
- Reject undeclared entity fields before persistence. Foreign-key errors include
  machine-readable `error` and `message` fields; entity and schema JSON rejections
  use the API error envelope. Internal storage diagnostics stay out of REST and
  GraphQL error messages, and database connection errors omit credentials.
- Keep authorization resource attributes aligned with the generated Cedar schema,
  so arrays of unsupported policy types do not incorrectly deny valid writes.
- Convert nested composite values using their declared field types before storage.

### Database and operator fixes

- Create SurrealDB records without supplied fields using valid statements, allowing
  schema defaults and required-field checks to run.
- Backfill missing SurrealDB values through a table scan so newly created indexes
  cannot omit rows during field additions and renames.
- Add PostgreSQL literal defaults with their columns so existing rows are filled.
  Required-field transitions backfill before enforcing `NOT NULL`; plans without
  a usable literal backfill value are refused before changes are applied.
- Report stored/derived inverse-collection transitions as destructive migrations.
  Cross-schema changes in collection meaning require a reviewed batch migration.
- PostgreSQL planning and inspection connections perform no bookkeeping DDL.
  Fresh databases plan as empty registries, and read-only roles can inspect existing
  metadata without schema creation privileges.
- PostgreSQL `contains` and `startswith` now match literal, case-sensitive text.
  Unsupported array filter comparisons return validation errors before execution.
- `apply` and `migrate --execute` preflight all selected migration plans before
  applying a noninteractive batch. Refusals identify every destructive schema and
  step requiring `--force`.
- Migration warnings display `review` when they are informational. New-table unique
  constraints are safe, and new schemas retain their `CREATE` label.
- Explicit listener flags override environment and file settings; omitted flags
  preserve configuration. An unconfigured server defaults to `127.0.0.1:3000`.
- Entity CLI requests retry HTTP 429 with `Retry-After` support and bounded fallback
  backoff, controlled by `--max-retries`. Transport failures are not retried.
- Document governor quotas, proxy configuration, probe routes, the full write-rule
  order, and webhook delivery guarantees.

### Generated sites

- Generate compilable list and detail pages for relation-only and file-only
  schemas. Primary relation cells and relation pickers use display labels, and
  nested display relations resolve through bounded, authorized lookups.
- Carry the active tenant on entity, invitation, and file requests, including
  requests retried after a token refresh.
- Show readable API errors and avoid duplicate global notifications when pages
  handle errors locally. Projects can customize the preserved error-toast helper.
- Generate typed duration, map, and base64 bytes fields in forms, lists, and
  details. Form validation and payload normalization respect computed fields and
  role-based field access. Composites with protected children remain read-only.
- Configure the product name, title suffix, and SVG logos and favicon through
  `[schema_forge.site]` or generation flags. Default marks are neutral, and CSS,
  title helpers, and SVG assets support template overrides and drift checking.
- CI now builds and lints generated TypeScript before running browser tests.

### Upgrade notes

Webhook consumers must support `payload_version: 2` and plain JSON field values.
Hidden fields and fields with field-access annotations are excluded. Webhooks
remain best effort with no durable history or replay; applications must reconcile
current state separately when delivery gaps matter. See [webhooks](docs/webhooks.md).

Before upgrading a tenanted deployment, annotate every application schema with its
intended tenant relationship and migrate existing ownership explicitly. Unannotated
application schemas are no longer implicitly shared when a tenant root exists.
See [tenant isolation](docs/tenant-isolation.md).

Migration safety serialization emits `Review`; legacy `RequiresConfirmation` input
is still accepted. Rust embedders must update webhook event constructor calls to
pass schema definitions, and handler callers must use the new JSON extractor.
Workspace crate versions are coordinated for the updated public core/backend types.

Regenerate sites to update owned API and branding helpers. Existing customized
page shells remain preserved; see the migration instructions for
[error feedback](docs/generated-site-errors.md) and [branding](docs/site-branding.md).

## [0.45.0] - 2026-09-24

### Security and correctness

- Tenant scope applies only to tenanted schemas. Tenant roots are authorized by
  their own identity, including legacy roots with NULL metadata. Unattributed
  child records fail closed for tenant users, and PUT/PATCH cannot move a record
  to another tenant unless the caller is a platform administrator. Invitations
  validate both the configured tenant type and the caller's effective scope.
- Field write authorization precedes defaults, computed expressions, validation
  rules, and hooks. Retained caller input is reauthorized after denied fields
  are removed. Rules see optional absent fields as null; required fields are
  checked after server-supplied values are available. Filtered empty PATCH
  requests no longer emit invalid SQL. PUT rules and persisted optional values
  now agree: PUT clears omitted writable optional fields and preserves denied
  or server-managed fields. PATCH remains a partial update.
- SurrealDB uses the supported 3.3 storage engine, correcting a concurrency race
  that could let two conditional writes both succeed.
- SQL Server updates merge supplied fields atomically, preserving omitted fields
  and explicit nulls. Concurrent updates to different fields no longer replace
  each other's stored values.
- Canonical mutation handlers enforce concrete Cedar authorization before
  operator policies and hooks. An operator policy can further restrict access
  but cannot bypass tenant or Cedar checks.
- GraphQL creates, updates, and deletes share the REST mutation pipeline. GraphQL reads,
  relations, and deletes enforce concrete Cedar decisions. Each request captures
  matching live definitions and policies, so runtime field restrictions also
  govern existing GraphQL fields. Nested relations honor operator visibility
  restrictions, and SurrealDB to-many relations retain their record references.
  Unproven raw
  GraphQL totals are withheld instead of disclosing counts of inaccessible rows.
- CLI help hides secret environment values, including database and server URLs.
  `serve --host` controls the actual listener and accepts validated IPv4/IPv6
  addresses. The default listener is now the documented `127.0.0.1`.

### Schema migrations

- Declare field renames with `@renamed_from("old_name")` to preserve data instead
  of dropping and recreating a field. Rename hints are validated before migration
  and remain valid after the rename completes.
- `serve` preflights startup plans and refuses destructive changes unless
  `--allow-destructive-migrations` is supplied. Runtime schema PUT requires
  `allow_destructive_migrations: true` for destructive plans, including lossy
  type conversions. Proposed tenant hierarchies and custom Cedar policies are
  validated before application schema changes.
- PostgreSQL to-one relation foreign keys are installed consistently for fresh,
  altered, and legacy schemas. Missing targets or orphan references fail schema
  administration clearly. Integrity violations return HTTP 409
  `foreign_key_violation` rather than 502, without exposing SQL or row values.
- Automatic changes to an existing schema's tenancy are refused because row
  ownership cannot be inferred safely. The
  [migration guide](docs/migrations/safe-schema-changes.md) documents explicit
  ownership backfill, constraint changes, and metadata updates.
- Explicit empty access lists produce diagnostics; documentation clarifies that
  within access annotations, empty and omitted role lists grant access to every
  authenticated user.

### Upgrade notes

Source builds use Rust 1.97.1. SurrealDB deployments require a stable 3.x server at version 3.3 or newer. Upgrade remote servers before
connecting this release; embedded development databases use the bundled engine.

Review pending schema changes before restarting. Destructive startup migrations
now require deliberate opt-in. PostgreSQL schema administration repairs missing
foreign keys and can require cleanup of existing orphan references. Tenanted
children created by a platform administrator require an explicit tenant.
Runtime changes to tenant topology require offline schema application and a
restart so the actor and HTTP middleware activate the same configuration.

Rust embedders must update the schema-aware tenant helper and rule-binding call
signatures. GraphQL registration now requires initialized
`AppState<SchemaForgeConfig>`; see [GraphQL writes](docs/graphql-writes.md).
The backend trait adds a defaulted `finalize_schema_migrations` operation for
completing batch migration integrity checks. Custom backends must implement
the atomic `apply_schema_change` operation to support runtime schema creation
and updates; the default refuses these operations rather than applying a
partial migration.

## [0.44.2] - 2026-09-10

### Fixed

- Removed an unnecessary JSON-payload gate from PostgreSQL exact authorized
  counts. In v0.44.1, a single broad, shallow JSON document could force a whole
  collection back to per-record scanning and restore the 30-second timeout
  reported in [#159](https://github.com/Govcraft/schemaforge/issues/159).
  JSON contents are absent from Cedar's resource representation and cannot
  affect a certified generated Read decision. Physical types, tenant isolation,
  and checks on represented authorization attributes remain enforced.
- Selected rows retain normal decoding and error handling. Undecodable JSON
  outside the selected page no longer fails an otherwise valid certified page
  and exact authorized total. Custom-policy fallback behavior is unchanged.
- Debug diagnostics identify failed authorization-value checks and
  fields without logging row values.

## [0.44.1] - 2026-09-10

### Fixed

- PostgreSQL list requests with generated Read policies can return exact totals
  without per-record Cedar evaluation after policy and storage checks establish
  equivalent authorization, including tenant isolation. Fixes the default-count
  timeout reported in [#159](https://github.com/Govcraft/schemaforge/issues/159).
  A synthetic 202,628-row fixture returned exact totals in approximately 0.6
  seconds for simple records and 0.8 seconds for richer records; the v0.44.0
  baseline timed out after 30 seconds.
- List requests capture one Cedar policy snapshot and reuse prepared principal
  and action state. The default Cedar adapter no longer evaluates each row
  twice. Explicit operator policies remain enforced alongside Cedar.
- Exact totals remain enabled by default. `count=false` still stops after the
  readable page is filled. Custom applicable Read policies, unsupported storage
  shapes, and other backends retain exact authorized scanning, which can still
  reach the request deadline on large collections. See
  [authorized pagination](docs/authorized-entity-pagination.md).

No schema or policy migration is required from v0.44.0. CLI/release version is
0.44.1; integration crate version is 0.43.1.

## [0.44.0] - 2026-09-09

### Fixed

- **`@owner` no longer hides records from the roles a schema grants read to.**
  The generated `forge.<schema>.owner_restrict` policy listed `Read` and
  `List` alongside `Update` and `Delete`. A Cedar `forbid` overrides every
  `permit`, so the annotation silently revoked the schema's own
  `@access(read: [...])` grant for every record the caller had not personally
  created, turning any shared workspace into per-user silos, with only
  `platform_admin` exempt. The policy now covers `Update` and `Delete` only;
  read access follows `@access(read:)`, tenant restrictions, and other
  applicable policies. Owner write behavior is unchanged: the server still stamps the creator
  (`inject_owner_on_create`), the value is still immutable after create
  (`strip_owner_on_update`), and the owner-write permit still lets a creator
  update their own record. **Fixes
  [#151](https://github.com/govcraft/schemaforge/issues/151).**
- **A `required` `@owner` field no longer answers `403` on the collection
  endpoint.** `build_resource_placeholder` populates required fields with type
  defaults, so a `required @owner` text field gave the schema-level
  placeholder `owner = ""`, which satisfied the old forbid's
  `resource has "<owner>"` conjunct and denied the whole collection. An
  optional owner field on an otherwise identical schema answered `200` with
  zero rows instead. Scoping the policy to the per-record actions removes the
  placeholder from that decision, so `required` and optional owner fields now
  behave identically. Hand-written policies that dereference a resource
  attribute still see those defaults on a schema preflight, and now separate
  the two cases with `context.resource_is_placeholder`
  ([#155](https://github.com/govcraft/schemaforge/issues/155)).

- Entity counts, offsets, and limits now apply after record-policy and Cedar authorization for both GET lists and POST queries. Totals count only readable records; pages no longer lose entries to filtering after pagination. Fixes [#152](https://github.com/Govcraft/schemaforge/issues/152). Exact totals scan matching candidates; see [authorized pagination](docs/authorized-entity-pagination.md) for cost and consistency limits.
- Cedar decision logs identify placeholder checks, resource UIDs, matched policy IDs, and evaluation errors. Effective rejections are logged as denials even when Cedar also reports a matching permit. Fixes [#155](https://github.com/Govcraft/schemaforge/issues/155).
- Upgraded all direct `acton-service` dependencies to published **0.43.1**. Generated request IDs now reach authentication, HTTP audit, and application events. Token validation events include method and path; completed HTTP events provide response status and duration when HTTP collection is enabled. Historical events are unchanged.

### Security / Breaking

- **Records on `@owner` schemas become visible to every role named in
  `@access(read: [...])`.** This is the intended behavior and the fix for
  [#151](https://github.com/govcraft/schemaforge/issues/151), but it *widens*
  read access relative to previous releases. A deployment that was relying on
  `@owner` to keep records private, whether deliberately or without realising
  it, will see those records become readable by the roles its own schema
  already granted. Audit any schema carrying `@owner` before upgrading, and
  see the migration note below to restore owner-only reads deliberately.

- Generated application and field actions now require Boolean `context.resource_is_placeholder`. SchemaForge's authorization entry points set it automatically. Manual Cedar callers must supply it from trusted server context; empty context fails strict request validation. Existing custom policies retain their behavior and must explicitly handle preflights where appropriate. See [custom policy context](docs/custom-policy-context.md).
- `@access(read:)` remains subject to tenant restrictions and other applicable custom forbids. The owner fix removes only the generated owner Read/List restriction; non-owner updates and deletes remain restricted.

### Migration

#### Restoring owner-only reads after the `@owner` fix (#151)

Most deployments need nothing: `@owner` was almost always used to stamp the
creator on a shared record, and the read restriction was an unintended side
effect. Check by listing the schemas that carry the annotation and reading
their `@access(read:)` grant. If the grant names roles that *should* see every
record, the new behavior is what the schema always said, and there is nothing
to do.

If a schema genuinely wants owner-only reads, say so explicitly with a custom
policy under `policies/custom`, which keeps the intent visible in review
rather than implied by a field annotation:

```cedar
@id("myapp.document.owner_only_read")
forbid (
    principal is Forge::Principal,
    action == Action::"ReadDocument",
    resource is Document
) when {
    !context.resource_is_placeholder
    && resource has "created_by"
    && (!(principal has id) || resource["created_by"] != principal.id)
    && !(principal in Forge::Group::"platform_admin")
};
```

The `!context.resource_is_placeholder` guard is what keeps the restriction on
the records instead of on the whole collection: without it, a schema preflight
against the synthetic resource reads `created_by = ""`, the forbid fires, and
the endpoint answers 403, the defect this release fixes, reintroduced by hand.
Scope the forbid to `Read` rather than `List`, because a collection request
preflights the `Read<Schema>` action at schema scope before checking each row.
See [Custom policies and authorization context](docs/custom-policy-context.md)
for the full context contract.

Run `schemaforge policies validate --custom-dir policies/custom` afterwards
to compile the bundle, including the custom policy, in strict mode before
deploying. Use the configured custom-policy directory if it differs.

#### Manual Cedar requests and custom policies (#155)

Set `context.resource_is_placeholder` to true only when authorizing a synthetic schema resource, and false for concrete resources and field checks. Do not derive it from client input. Guard Read restrictions with `!context.resource_is_placeholder`; a conditional permit must separately admit the preflight. Do not copy that guard to Create expecting proposed-field validation: the generic Create route currently performs schema authorization without a concrete proposed entity.

### Versions

- Product and CLI: **0.44.0**.
- Independently versioned integration crate: **0.43.0** (`build.version`).
- CLI deployments report **0.44.0** as `build.release_version`, plus the release source revision.
- Framework dependency: **acton-service 0.43.1**, resolved from crates.io.

## Earlier release notes (legacy unversioned entries)

### Added

- **Signed-schema enforcement.** New `schema-forge-signing` crate verifies
  per-file digital signatures and a signed directory manifest before any
  `.schema` file is parsed by `apply`, `migrate`, `serve`, `parse`,
  `export`, `policies`, `hooks`, or `site`. The trust policy lives under
  `[schema_forge.signing]` in `config.toml`; three signer kinds are
  defined — `ed25519` (Phase 1, shipped), `ssh-allowed-signers` (Phase 2,
  shipped), and `cosign-keyless` (Phase 3, shipped). Trust evaluation uses
  OR-semantics so rotating keys is additive. Three modes: `off` (default
  for now, preserves pre-signing behaviour), `warn` (run checks, log
  failures, continue), `enforce` (any failure aborts with exit code 13).
  Two new subcommands wrap the verifier:
    - `schemaforge sign <paths>` — produce per-file `.sig` files and a
      signed `schemas.manifest.toml`. `--ed25519-generate` creates a
      fresh keypair; `--ed25519-key` reuses one; `--ssh-key` signs with
      an existing OpenSSH private key (SSHSIG format, identical to
      `ssh-keygen -Y sign`); `--keyless` shells out to `cosign
      sign-blob --bundle …` so the on-disk `.sig` becomes a Sigstore
      Bundle JSON ready for offline verification; `--print-pubkey`
      emits the trust-anchor block matching the chosen scheme.
      `--ssh-principal <id>` customises the principal label printed in
      the allowed-signers advisory output. `--cosign-bin <path>`
      overrides the `cosign` binary location used by `--keyless`.
    - `schemaforge verify <paths>` — standalone verifier suitable as a
      pre-merge CI gate; touches no database.

  Two new global flags route through the verifier:
    - `--trust-policy <path>` overrides `[schema_forge.signing]` with a
      standalone TOML, so one shared `config.toml` can fan out to many
      environments without duplicating database settings.
    - `--no-verify` skips verification for one invocation, but is
      *refused* when `signing.mode = "enforce"` unless
      `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set — production deployments
      cannot silently skip verification.

  Defeats two threat classes that an unsigned schema directory leaves
  open: (1) filesystem-level tampering of any `.schema`, (2) introduction
  of untrusted authors via "drop a file in `schemas/`." Per-file
  detached signatures cover tampering; the signed manifest with pinned
  SHA-256s and an explicit file list covers add/remove attacks.

  Phase 2 adds the **SSH allowed_signers** verifier: trust roots can now
  point at an `allowed_signers` file (the same format `git config
  gpg.ssh.allowedSignersFile` consumes), and signatures live as
  PEM-armored SSHSIG blobs under namespace
  `schema-forge-signing@govcraft.ai`. Supports the
  `namespaces="..."`, `valid-after`, and `valid-before` per-line options,
  so a key rotated out of date or restricted to a different namespace is
  rejected at the policy layer before any cryptographic check runs.

  Phase 4 adds **offline Sigstore trust-root** support for SCIF /
  airgap deployments. The `[schema_forge.signing] trust_root_bundle =
  "/path/to/trust_root.json"` field — accepted but inert in earlier
  phases — now drives every `cosign-keyless` verifier in the policy:
  one shared `TrustedRoot` is loaded from disk at startup and cloned
  into each verifier instead of the embedded production snapshot. A
  new `schemaforge trust-bundle refresh` command does a full TUF
  fetch on a connected host (selectable target: `public-good`,
  `staging`, or `github`) and writes the resulting JSON to disk; the
  operator copies that file across the airgap and points
  `trust_root_bundle` at it. `trust-bundle inspect` prints a one-line
  fulcio/rekor/TSA count summary so the operator can confirm a sane
  snapshot before deploying. The verifier fails loud if the
  configured bundle path is missing or malformed — silent fallback to
  the embedded snapshot would hide rotation drift, which is the whole
  reason this knob exists.

  Phase 3 adds the **cosign-keyless** verifier so the same CI identity
  that already signs SchemaForge releases can sign schemas. Trust roots
  point at an OIDC `issuer` plus a glob `subject_pattern`; verification
  rides the `sigstore-verify` crate's full chain (cert ↔ Sigstore Fulcio
  root, SCT, Rekor inclusion proof, signature) and then post-checks the
  cert's OIDC subject against the operator glob. On disk, the `.sig`
  next to each schema is a Sigstore Bundle (`mediaType
  application/vnd.dev.sigstore.bundle.v0.3+json`) rather than the legacy
  `.sig`+`.pem` pair — bundles embed the Rekor inclusion proof, which
  preserves the historical signing time the (10-minute) Fulcio cert
  needs to validate long after expiry. Signing uses `schemaforge sign
  --keyless`, which shells out to `cosign sign-blob --yes --bundle`; we
  do not reimplement the Fulcio/Rekor OIDC dance because `cosign` is the
  canonical CLI for that flow and ships in every Sigstore-enabled CI
  environment. A new `--cosign-bin` overrides the binary location for
  runners with a non-standard install.

- New `fips` cargo feature on `schema-forge-cli` (and `schema-forge-acton`)
  routes rustls through `aws-lc-rs` compiled against the FIPS-validated
  AWS-LC C library (`aws-lc-fips-sys`). At startup, the CLI installs
  `aws_lc_rs` as the process-wide rustls `CryptoProvider`, so PostgreSQL
  (sqlx), S3 (reqwest), and the hook dispatcher (tonic) all terminate
  TLS through the FIPS module. Pair with `postgres`; the `surrealdb`
  backend still pulls `rustls/ring` transitively and is not FIPS-clean.
  Build requires CMake, a C/C++ toolchain, and Go 1.18+. See README
  "FIPS builds".

- Operator reference doc `docs/signing-reference.md` covers the
  signed-schema subsystem end-to-end: threat model, trust-policy TOML
  schema, manifest format, every CLI flag (sign / verify / trust-bundle
  refresh / trust-bundle inspect), the off → warn → enforce rollout
  playbook, and the airgap / SCIF workflow. Linked from the README
  Signed-Schema Enforcement section.

### Changed

- Upgraded `acton-service` to **0.39.0** in `schema-forge-acton`,
  `schema-forge-backend`, `schema-forge-cli`, and `schema-forge-mssql`. The
  release is additive: it introduces a SAML 2.0 service provider behind a new
  `saml` feature and changes nothing in the features this workspace already
  enables. SchemaForge does **not** turn `saml` on yet — acton-service ships
  the SP as a library (`SamlServiceProvider`, config, replay/pending stores),
  not as mounted routes, so consuming it means wiring
  `/saml/metadata`, `/saml/login`, and `/saml/acs`, plumbing
  `[auth.saml]` config, and deciding how an assertion maps onto a `User`
  entity and a tenant. Tracked separately.
- Upgraded `acton-service` to **0.26** in `schema-forge-acton`,
  `schema-forge-cli`, and `schema-forge-backend`. 0.26's new
  `crypto-aws-lc-rs` feature (enabled by default in our build) propagates
  `aws-lc-rs` through `rustls`, `tokio-rustls`, `reqwest`, `sqlx`, and
  `tonic`, replacing the previous ring-backed default.
- `schema-forge-postgres` no longer pins `sqlx`'s `tls-rustls` (ring)
  feature; `acton-service`'s crypto feature drives the TLS provider so
  the FIPS path can swap cleanly.

### Fixed

- **`enum`, `text(max:)`, and `integer(min:/max:)` are now enforced
  in-process.** They were declared in the DSL but never checked before the
  write reached the database, so the only thing refusing them was the
  generated `CHECK` constraint (or `VARCHAR(n)`) — and a check violation is
  not mapped to a client error. A caller sending `kind: "gamma"` to an
  `enum("alpha", "beta")` field got `502 backend_unavailable`, a retryable
  status for a request that could never succeed, with a raw driver message
  and no field name. `FieldType::check_value` now checks every declared
  constraint and the write path answers `422 validation_failed` naming the
  field and, for an enum, the allowed variants. The check runs at the last
  seam before the backend, so it also covers values produced by `@default`
  and `@compute` rules and by `before_*` hooks — not only client JSON.
  Nothing about which writes succeed changes; only the status code, the
  message, and where the refusal happens.
  **Fixes [#133](https://github.com/Govcraft/schemaforge/issues/133).**
- **`unique` on a `@tenant(root)` schema is enforced again.** The unique
  index was scoped to `(_tenant, field)` for every schema carrying any
  `@tenant` annotation, root included. On a root schema that does not weaken
  the constraint, it removes it: `_tenant` is NULL on a platform-level
  create and PostgreSQL treats NULLs as distinct, so two organizations could
  carry the same `short_code` and the migration would still apply cleanly.
  Root-tenant rows are also the rows most likely to need global uniqueness —
  they *are* the tenants, so their identifying fields have to be unique
  table-wide by definition, and there is no outer tenant to scope them to.
  The scoping decision now runs off the new
  `SchemaDefinition::unique_scoped_by_tenant`, which is true only for
  `@tenant(parent: ...)`. `is_tenanted` keeps its old meaning and still
  drives the `_tenant` column; the two questions were being answered by one
  predicate. Affects the PostgreSQL and SurrealDB backends alike.
  **Fixes [#134](https://github.com/Govcraft/schemaforge/issues/134).**
- `schema-forge-backend` was still pinned to `acton-service 0.23` while
  the rest of the workspace had moved to 0.26.1. The dual-version
  trait mismatch refused to compile (`filter_visible`/`can_modify`/
  `can_delete` signatures diverged between the two acton versions).
  Aligned backend to 0.26.1 with `crypto-aws-lc-rs` enabled.
- `commands/site/mod.rs` referenced an `accessibility_contact` field on
  the public `SiteContext`/`PageContext` structs and rendered
  `src/pages/accessibility.tsx`, but the field and template had never
  shipped. Every `site generate` invocation crashed with "site template
  not registered" and ten `site_generate` tests failed. Restored the
  fields and a §508 / OMB M-24-08 conformance-statement template
  rendering a visible "not configured" notice when the contact is
  unset, so the audit gap remains visible instead of silently passing.

### Security / Breaking

- `schemaforge serve` no longer auto-seeds the five SchemaForge demo personas
  (alice, bob, charlie, dana, eve — each with the literal password
  `"password"`) when bootstrapping the admin user via `--admin-user` /
  `--admin-password` (or `FORGE_ADMIN_USER` / `FORGE_ADMIN_PASSWORD`). The
  legacy behavior fired automatically whenever the user store was empty before
  bootstrap, leaving downstream AMI/customer deployments with six accounts and
  five default passwords. **Fixes [#53](https://github.com/govcraft/schemaforge/issues/53).**
- Demo persona seeding is now strictly opt-in via the new
  `--seed-demo-users` flag (env: `FORGE_SEED_DEMO_USERS`, default: `false`).
  The bundled `task demo` flow passes the flag explicitly; `task serve` does
  not.
- `schema_forge_acton::shared_auth::bootstrap_demo_users` gained a required
  `seed: bool` parameter. Existing callers in operator code must pass `false`;
  only controlled local-development flows should pass `true`.
- `SchemaForgeExtensionBuilder` gained `with_seed_demo_users(bool)`. The
  default is `false`, matching the new safe-by-construction posture.

### Migration

#### `unique` on a tenant root (#134)

New databases need nothing. An **existing** database applied before this fix
still carries the tenant-scoped index, and the schema diff cannot see the
difference: the stored schema is unchanged, so no `AddUnique` step is
emitted and the stale index stays. The scope changed in the code, not in the
schema.

Check for it, then replace it. For each `@tenant(root)` schema with a
`unique` field, on PostgreSQL:

```sql
-- Confirm the stale shape: indexdef will name (_tenant, <field>).
SELECT indexdef FROM pg_indexes WHERE indexname = 'uq_Organization_short_code';

-- Find duplicates the broken index let through, and resolve them first —
-- the ALTER below will fail while any remain.
SELECT short_code, count(*) FROM "Organization"
GROUP BY short_code HAVING count(*) > 1;

DROP INDEX "uq_Organization_short_code";
ALTER TABLE "Organization"
  ADD CONSTRAINT "uq_Organization_short_code" UNIQUE ("short_code");
```

The old index and the new constraint share a name, so drop before adding.
On SurrealDB the equivalent is `REMOVE INDEX uq_Organization_short_code ON
Organization;` followed by the `DEFINE INDEX ... FIELDS short_code UNIQUE;`
that `schemaforge apply` would now emit.

Run the duplicate query before scheduling the change. A deployment that has
been live on the broken index may already hold rows that global uniqueness
would reject, and that is a data decision, not a migration step.

#### Constraint violations now return 422 (#133)

No schema or config change. Clients that were treating a `502` from a write
as "backend down, retry" will now see `422` for a value that violates a
declared `enum`, `text(max:)`, or `integer(min:/max:)` constraint. That is
the point — the request was never going to succeed — but any retry logic
keyed on the old status should be checked.

Projects that worked around this by declaring a `@require` CEL rule
alongside the column constraint (`integer(min: 1, max: 5) required
@require("size >= 1 && size <= 5", "...")`) can drop the rule; the column
declaration now produces a `422` on its own. Keeping it is harmless — it
simply fires first, with its own message.

#### Demo-user seeding (security fix)

Operators upgrading from `schema-forge-cli` 0.27.x:

- If you were relying on the implicit demo seed for local development, add
  `--seed-demo-users` (or set `FORGE_SEED_DEMO_USERS=true`) to your serve
  invocation. The `task demo` Taskfile entry has already been updated.
- If your deployment was unintentionally inheriting the demo accounts, remove
  them with `schemaforge user delete <name>` (or your backend's equivalent) and
  rotate any passwords that may have leaked. They were created with the literal
  string `"password"`.

#### Signed-schema rollout (off → warn → enforce)

Adopting signing on an existing deployment is a three-stage migration. The
scaffold from `schemaforge init` defaults to **stage 1** with the
`[schema_forge.signing]` block fully commented out — `mode` defaults to
`"off"` and pre-signing behaviour is preserved. Once a deployment is ready:

1. **Generate keys and sign every schema.** Pick one of the three signer
   kinds (ed25519, SSH allowed_signers, cosign-keyless) and run
   `schemaforge sign schemas/ --print-pubkey` to produce the trust-anchor
   block. Paste the printed `[[schema_forge.signing.trusted_signers]]`
   entry into `config.toml`.
2. **Move to `mode = "warn"`.** Uncomment the signing block. Every command
   now runs the full verifier (manifest signature, per-file signatures,
   disk-vs-manifest cross-check, pinned SHA-256s) but logs failures
   instead of aborting. Use this stop to flush out config / CI-pipeline
   gaps without breaking production. The shipped scaffold sets
   `mode = "warn"` as the recommended starting point when the block is
   uncommented.
3. **Promote to `mode = "enforce"`.** Once `schemaforge verify` is green
   in CI and every operator command exits 0, change the line to
   `mode = "enforce"`. Verification failures now abort with exit code
   13. `--no-verify` is refused under enforce unless
   `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set — production deployments
   cannot silently skip verification.

Airgap / SCIF deployments using `cosign-keyless` should also seed the
offline trust root before flipping the mode: run `schemaforge trust-bundle
refresh --output trust_root.json` on a connected host, copy the file across
the airgap, and set `trust_root_bundle = "/path/to/trust_root.json"`. The
verifier loads that snapshot at startup and uses it for every
`cosign-keyless` anchor; missing or malformed bundles fail loud rather
than silently falling back to the (eventually-stale) embedded snapshot.

### Version bumps

- `schema-forge-cli`: 0.27.0 → 0.29.0
- `schema-forge-acton`: 0.26.0 → 0.28.0
- `schema-forge-backend`: 0.10.0 → 0.11.0
