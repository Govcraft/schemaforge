# SchemaForge — Configuration Reference

Schema-forge does **not** maintain its own config layer. All runtime configuration goes through acton-service's canonical `Config<SchemaForgeConfig>` — schema-forge-specific fields live under `[schema_forge.*]` (the `T` parameter), everything else uses acton-service's standard sections.

## Discovery order

Highest priority first:

1. `--config <PATH>` flag (passes through to `acton_service::Config::load_from`)
2. Acton's XDG search: `./config.toml`, `~/.config/acton-service/schemaforge/config.toml`, `/etc/acton-service/schemaforge/config.toml`
3. `ACTON_*` env vars layer on top of whatever file was loaded (highest priority below the CLI flags)

## config.toml — annotated example

```toml
# SurrealDB backend — a SurrealDB-flavored URL goes here.
[surrealdb]
url = "ws://localhost:8000"
namespace = "schemaforge"
database = "dev"
# Optional credentials (or set ACTON_SURREALDB_USERNAME / _PASSWORD env vars)
# username = "root"
# password = "..."

# PostgreSQL backend — uncomment to switch (mutually exclusive with [surrealdb]).
# [database]
# url = "postgres://user:pass@localhost:5432/schemaforge"
# max_connections = 50
# min_connections = 5

[token]
format = "paseto"
version = "v4"
purpose = "local"
key_path = "./keys/paseto.key"
issuer = "schemaforge"

# Storage backends for `file` field types. Each schema `file(bucket: "NAME")`
# declaration must resolve to a backend declared here, or startup fails.
[schema_forge.storage]
default_presign_ttl_secs = 300

[schema_forge.storage.backends.documents]
endpoint = "http://127.0.0.1:9100"       # MinIO in dev; omit for AWS regional
region = "us-east-1"
bucket = "forge-documents"
access_key_id = "${S3_ACCESS_KEY}"
secret_access_key = "${S3_SECRET_KEY}"
force_path_style = true                   # required for MinIO
presign_ttl_secs = 300

# Bulk-export hardening bounds (ADR-0003). Both are fail-closed and optional;
# the defaults preserve the ADR's example behaviour. See export.md.
[schema_forge.export]
default_max_rows = 100000                 # server-wide row ceiling; a schema's
                                          # @export(max_rows) is intersected (min)
                                          # with this — schemas narrow, never widen

[schema_forge.export.rate_limit]
max_requests = 30                         # export initiations per subject per window;
                                          # 0 disables export entirely (kill switch)
window_secs  = 60                         # fixed-window length, in seconds

# Principal claim → Cedar attribute mappings. Each subsection name becomes
# an optional attribute on `Forge::Principal`; custom Cedar policies must
# guard reads with `principal has X && ...`. See principal-claims-reference.md.
[schema_forge.authz.principal_claims.client_org_id]
type     = "string"
required = true                           # daemon must populate or refuse login
source   = { user_field = "client_org_id" }   # IN-side: project User column at login

[schema_forge.authz.principal_claims.team_ids]
type   = "set_of_string"
source = { user_field = "team_ids" }      # text[] on User → set_of_string in token

[schema_forge.authz.principal_claims.tier]
type     = "long"
required = true                           # token missing this claim → 401
# no `source` — bearer/CLI supplies it out-of-band

# Signed-schema enforcement. The fresh scaffold from `schemaforge init`
# ships this block fully commented out — defaults to `mode = "off"` and
# pre-signing behavior is preserved. Uncomment and stage off → warn →
# enforce. See signing-reference.md for the full trust-anchor schema.
[schema_forge.signing]
mode = "warn"   # one of: off | warn | enforce
# trust_root_bundle = "/etc/schemaforge/trust_root.json"   # airgap only; affects cosign-keyless verifiers

# OR-semantics across signers — a file passes if any verifier accepts.
[[schema_forge.signing.trusted_signers]]
kind = "cosign-keyless"
name = "schemaforge-release-ci"
issuer = "https://token.actions.githubusercontent.com"
subject_pattern = "https://github.com/govcraft/schemaforge/.github/workflows/release.yml@refs/tags/v*"

[[schema_forge.signing.trusted_signers]]
kind = "ed25519"
name = "roland-airgap-key"
public_key_b64 = "MCowBQYDK2VwAyEA..."

[[schema_forge.signing.trusted_signers]]
kind = "ssh-allowed-signers"
path = "/etc/schemaforge/allowed_signers"
```

## Backend selection

By section: `[database]` → PostgreSQL, `[surrealdb]` → SurrealDB. Declaring both is a startup error (the operator must remove one or override with `--db-url`). Neither declared falls back to a dev SurrealDB at `ws://localhost:8000` for zero-config development.

**CLI overrides on the canonical config**: `--db-url <URL>` rewrites the matching section in-place (postgres URL → `[database].url`, anything else → `[surrealdb].url`) **and clears the other section** so acton-service can never spawn a leftover pool against a different backend. `--db-ns` / `--db-name` override `[surrealdb].namespace` / `.database`. Pool-sizing knobs the operator set in config.toml (`max_connections`, retries, etc.) survive the URL override — only the URL is rewritten.

## Environment Variables

Acton-service-native overrides use the `ACTON_*` prefix (these are the canonical env vars):

| Variable | Purpose |
|----------|---------|
| `ACTON_DATABASE_URL` | Override `[database].url` (PostgreSQL) |
| `ACTON_SURREALDB_URL` | Override `[surrealdb].url` |
| `ACTON_SURREALDB_NAMESPACE` | Override `[surrealdb].namespace` |
| `ACTON_SURREALDB_DATABASE` | Override `[surrealdb].database` |
| `ACTON_SURREALDB_USERNAME` | SurrealDB credentials (replaces the removed `SCHEMA_FORGE_DB_USER`) |
| `ACTON_SURREALDB_PASSWORD` | SurrealDB credentials (replaces the removed `SCHEMA_FORGE_DB_PASS`) |

Schema-forge CLI-flag aliases (clap `env = "..."` mappings; equivalent to passing the flag):

| Variable | Equivalent flag | Purpose |
|----------|-----------------|---------|
| `SCHEMA_FORGE_DB_URL` | `--db-url` | Connection URL; backend is auto-detected from the scheme |
| `SCHEMA_FORGE_DB_NS` | `--db-ns` | SurrealDB namespace |
| `SCHEMA_FORGE_DB_NAME` | `--db-name` | SurrealDB database name |
| `SCHEMA_FORGE_CONFIG` | `--config` | Config file path |
| `FORGE_ADMIN_USER` | `--admin-user` | Seed admin username (bootstraps the PASETO login store on first run; user is granted `["platform_admin"]`) |
| `FORGE_ADMIN_PASSWORD` | `--admin-password` | Seed admin password |
| `FORGE_SEED_DEMO_USERS` | `--seed-demo-users` | Opt-in to demo persona seeding (alice/bob/charlie/dana/eve, each with literal password `"password"`). Default `false`. Bundled `task demo` passes this; `task serve` does not. (Strictly opt-in since v0.29.0 / fix #53.) |
| `SCHEMAFORGE_ALLOW_NO_VERIFY` | (paired with `--no-verify`) | When `1`, permits `--no-verify` to proceed under `mode = "enforce"`. One-off recovery only; never set in steady-state CI |

## Migration notes

### v0.21.0 (breaking)

- The old hybrid `[database] url = "ws://..."` (URL-scheme-detected) layout is gone — move SurrealDB URLs to `[surrealdb]`, leave `[database]` for PostgreSQL.
- The `[cli]` section (`default_schema_dir` / `default_policy_dir`) was never read at runtime; remove it.
- `SCHEMA_FORGE_DB_USER` / `SCHEMA_FORGE_DB_PASS` env vars are removed; use `ACTON_SURREALDB_USERNAME` / `ACTON_SURREALDB_PASSWORD`.
- The bootstrap admin user is now granted `platform_admin` (not `admin`).

### v0.22.0 (breaking)

- Authorization is now Cedar-canonical end-to-end. The legacy `Permission` and `Role` system schemas have been removed; their data was never used at runtime once the Cedar engine landed. Drop them from any custom seed scripts.
- The legacy `_forge_users` parallel store is gone. User accounts live in the canonical system `User` schema and are read through `EntityAuthStore`. First-run provisioning now goes through `schema-forge bootstrap-admin` (or the existing `--admin-user` / `FORGE_ADMIN_USER` seeding on `serve`, which was rewired to `EntityAuthStore`). Existing `_forge_users` rows must be migrated into the `User` table — there is no automatic backfill.
- Custom Cedar policies (under `policies/custom/`) are now strict-mode-validated on every load. Policies that compiled under the previous lenient mode but reference unknown attributes / actions / entity types will fail validation; run `schema-forge policies validate` to surface every issue at once.
- Add `policies/role_ranks.toml` with the operator-controlled rank for any custom role you reference in policies. Missing ranks fail the bundle. `platform_admin` is reserved and cannot appear in this file.

### v0.29.0 (breaking + new subsystem)

- **Signed-schema enforcement** lands as an opt-in subsystem under `[schema_forge.signing]`. New `mode` field (`off` / `warn` / `enforce`), `trusted_signers` array (OR-semantics across `ed25519` / `ssh-allowed-signers` / `cosign-keyless`), and optional `trust_root_bundle` path for airgap. Default mode is `off`, so existing deployments are unaffected until an operator stages the off → warn → enforce rollout. See [signing-reference.md](signing-reference.md) for the full schema.
- **Demo persona seeding is strictly opt-in.** `schemaforge serve` no longer auto-seeds the five demo personas (alice/bob/charlie/dana/eve, each with literal password `"password"`) when bootstrapping the admin user. The legacy implicit seed shipped six accounts and five default passwords into every AMI / customer deployment that hit an empty user store. Set `--seed-demo-users` / `FORGE_SEED_DEMO_USERS=true` for local-dev only. Fixes [#53](https://github.com/govcraft/schemaforge/issues/53). `SchemaForgeExtensionBuilder::with_seed_demo_users(bool)` and the new required `seed: bool` parameter on `schema_forge_acton::shared_auth::bootstrap_demo_users` are breaking signature changes — existing callers must pass `false`.
- **`acton-service` upgraded to 0.26.1** workspace-wide. Pulls `aws-lc-rs` through `rustls`, `tokio-rustls`, `reqwest`, `sqlx`, and `tonic`, replacing the previous ring-backed default. The `crypto-aws-lc-rs` feature is enabled by default in the SchemaForge build.
- **New `fips` cargo feature** on `schema-forge-cli` and `schema-forge-acton` routes rustls through `aws-lc-rs` compiled against the FIPS-validated AWS-LC C library. Pair with `postgres`; the `surrealdb` backend pulls `rustls/ring` transitively and is **not** FIPS-clean. Build requires CMake, a C/C++ toolchain, and Go 1.18+.

## policies/role_ranks.toml

The role-name → numeric-rank map that gates user-mgmt and any policy that compares `principal.role_rank` against `resource.role_rank`. Lives in version control alongside the policies it governs. Missing file is treated as "platform_admin only".

```toml
# policies/role_ranks.toml
#
# Numeric ranks define the no-upward-visibility hierarchy. A principal can
# manage / see another user only when principal.role_rank >= target.role_rank.
# `platform_admin` is hardcoded to i64::MAX and MUST NOT appear here.

[roles]
admin    = 1000
manager  = 500
member   = 100
```

Validate the bundle (policies + ranks) before committing:

```
schema-forge policies validate --custom-dir policies/custom/ --role-ranks policies/role_ranks.toml
```

## Database Backends

### SurrealDB (default feature)

The default backend. Uses WebSocket (ws://) or HTTP connections with namespace/database selection.

```
schema-forge serve --db-url ws://localhost:8000 --db-ns myapp --db-name prod
```

### PostgreSQL (postgres feature)

Available when built with `--features postgres`. Uses connection URL with embedded credentials. Creates real PostgreSQL tables with proper types, CHECK constraints, indexes, and foreign keys.

```
schema-forge serve --db-url postgres://user:pass@host:5432/dbname
```

The two backends are **mutually exclusive** at build time (enforced by acton-service). The binary ships with one or the other.
