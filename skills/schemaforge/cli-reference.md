# SchemaForge — CLI Reference

The `schema-forge` binary (`schema-forge-cli` crate) is built with clap derive and routes all configuration through `acton_service::Config<SchemaForgeConfig>`.

## Global Options

All commands accept these flags:

| Flag | Env Var | Purpose |
|------|---------|---------|
| `-c, --config <PATH>` | `SCHEMA_FORGE_CONFIG` | Config file path |
| `--format <human\|json\|plain>` | — | Output format (default: human) |
| `-v, --verbose` | — | Increase verbosity (-v, -vv, -vvv) |
| `-q, --quiet` | — | Suppress non-error output |
| `--no-color` | `NO_COLOR` | Disable colored output |
| `--db-url <URL>` | `SCHEMA_FORGE_DB_URL` | Database connection URL (auto-detects backend from scheme) |
| `--db-ns <NS>` | `SCHEMA_FORGE_DB_NS` | Database namespace (SurrealDB only) |
| `--db-name <NAME>` | `SCHEMA_FORGE_DB_NAME` | Database name (SurrealDB only) |
| `--trust-policy <PATH>` | — | Override `[schema_forge.signing]` with a standalone TOML; lets one `config.toml` fan out to many environments |
| `--no-verify` | `SCHEMAFORGE_ALLOW_NO_VERIFY` | Skip signed-schema verification for one invocation. Refused under `mode = "enforce"` unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set |

**Backend auto-detection:** `postgres://` or `postgresql://` URLs select PostgreSQL. Everything else (ws://, wss://, mem://, http://, https://) selects SurrealDB.

**Signed-schema verification.** Every command that consumes `.schema` files (`apply`, `migrate`, `serve`, `parse`, `export`, `policies`, `hooks`, `site`) routes through the `schema-forge-signing` verifier before parsing. Behavior is driven by `[schema_forge.signing] mode` in `config.toml` (`off` / `warn` / `enforce`). Under `enforce`, a bad signature / hash / manifest aborts with exit code **13**. See `signing-reference.md`.

## Commands

### `schema-forge init <NAME>`

Initialize a new project directory with scaffold files.

```
schema-forge init my-project
schema-forge init my-project -t minimal    # minimal template
schema-forge init my-project -t api-only   # API-only template
schema-forge init my-project -y            # skip prompts, use defaults
schema-forge init my-project -f            # force overwrite existing dir
```

Templates: `full` (default), `minimal`, `api-only`.

### `schema-forge parse [PATHS...]`

Parse and validate `.schema` files without applying to a database.

```
schema-forge parse                     # default: schemas/
schema-forge parse src/schemas/
schema-forge parse --print             # show round-trip DSL output
schema-forge parse --debug             # show token-level parse info
schema-forge parse --format json       # JSON output for tooling
```

`parse` also **syntax-validates** every `@require`/`@compute`/`@default` CEL expression, reporting any CEL parse error mapped to its `line:column` in the `.schema` source.

### `schema-forge apply [PATHS...]`

Parse schemas and apply to a running database backend. Computes diffs against stored metadata and runs migrations.

On top of parse-time checks, `apply` **type-checks** each rule expression against the schema's field types (a definitely-incompatible expression — e.g. comparing an integer field to a string literal — is rejected as a `RuleTypeError` at `line:column`) and validates `related.<field>.<col>` cross-entity references (a `related.*` in `@compute`/`@default`, or a to-many / non-relation target, is rejected here).

```
schema-forge apply                              # apply schemas/ to default backend
schema-forge apply --db-url postgres://user:pass@host/db   # PostgreSQL
schema-forge apply --db-url ws://localhost:8000  # SurrealDB
schema-forge apply --dry-run                     # show plan without executing
schema-forge apply --force                       # skip confirmation for destructive changes
schema-forge apply --with-policies               # auto-generate Cedar policies
```

### `schema-forge migrate [PATHS...]`

Plan and optionally execute schema migrations. Dry-run by default.

```
schema-forge migrate                        # show migration plan (dry-run)
schema-forge migrate --execute              # apply the plan
schema-forge migrate --schema Contact       # plan for a specific schema only
schema-forge migrate --execute --force      # skip destructive change confirmation
```

### `schema-forge serve`

Start the HTTP server with the SchemaForge extension via acton-service.

```
schema-forge serve                                         # default: localhost:3000
schema-forge serve -H 0.0.0.0 -p 8080                     # custom host/port
schema-forge serve --db-url postgres://user:pass@host/db   # PostgreSQL backend
schema-forge serve --db-url ws://localhost:8000             # SurrealDB backend
schema-forge serve --schemas src/schemas/                   # custom schema directory
schema-forge serve --watch                                  # hot-reload (not yet implemented)
schema-forge serve --log-level debug                        # log level override
schema-forge serve --admin-user admin --admin-password secret  # bootstrap admin credentials
```

Environment variables for admin: `FORGE_ADMIN_USER`, `FORGE_ADMIN_PASSWORD`.

The HTMX site surface was removed in commit `fdd4976`. The site UI is now a separate React + Vite + Tailwind + shadcn project generated by `schema-forge site generate`. The backend serves only the REST API and auth endpoints — it does not serve HTML.

### `schema-forge site generate`

Generates a standalone React app that talks to the running `schemaforge serve` instance via `/api/v1/forge/*`:

```
schema-forge site generate -s schemas -o site            # scaffold into ./site
schema-forge site generate --schema Order                # single schema only
schema-forge site generate --check                       # dry-run; exits non-zero on drift
schema-forge site generate --templates-dir ./site-templates  # override bundled .jinja templates
schema-forge site generate --force-user-files            # rare: re-scaffold Preserve shells too
```

Layout:

- `src/app/pages/<kebab>/{list,detail,edit}.tsx` — **Preserve** shells under `/app/*`. Thin files that import schema-driven symbols from their `.generated.tsx` sibling and compose the final page. Users own layout, charts, custom state, mutation intercepts. Scaffolded once; left alone on regen.
- `src/app/pages/<kebab>/{list,detail,edit}.generated.tsx` — **Owned** schema-driven siblings. Carry `columns`, `SORTABLE_FIELDS`, `FILTERABLE_FIELDS`, `ENUM_COLORS`, `<EntityFormFields>`, `<EntityDetailRows>`, `normalize*InitialValues`, `normalize*Payload`. Rewritten on every run so schema edits flow through automatically without touching the preserve shell. This is the #40 split — you should never need `--force-user-files` just to pick up new columns or form fields.
- `src/admin/*` — **Owned** runtime-dynamic admin shell mounted at `/admin/*`. Uses `describeSchema` + `listEntities` to render any schema the user has read access to, without per-entity codegen.
- `src/generated/*` — **Owned** typed API client, entity types, zod schemas, route manifest, formatters. Regenerated every run.
- `src/components/ui/*` — **Owned** vendored shadcn primitives (button, input, card, form, table, relation-select, error-block).
- `src/lib/auth.ts` — **Owned** PASETO token store, login, refresh scheduler.
- `Cargo.toml`-equivalent files (`package.json`, `src/main.tsx`, etc.) are **Owned**. User-land code lives in the per-entity Preserve shells under `src/app/pages/**`.

Use `--force-user-files` only when you deliberately want to reset the preserve shells back to the default scaffold — e.g. after a major template change you want to pick up, or to abandon experimental customizations. The common "I changed a schema" workflow needs no flag.

Use `--templates-dir` to shadow any `.jinja` file in the site templates tree without rebuilding the CLI; files present there override the baked-in templates. Iterate on a template, re-run `schema-forge site generate`, `pnpm dev`, and see the change immediately.

### `schema-forge inspect [SCHEMA]`

Inspect registered schemas from the backend.

```
schema-forge inspect                    # list all schemas
schema-forge inspect Contact            # show specific schema details
schema-forge inspect Contact --detail   # detailed field information
schema-forge inspect --counts           # include entity counts per schema
schema-forge inspect --format json      # JSON output
```

### `schemaforge login`

Authenticate against a **running** instance over HTTP and cache a Bearer PASETO token. This is the credential path (username + password → `POST /api/v1/forge/auth/login`), distinct from `token generate`, which mints a token offline from the PASETO key. The token is cached `0600` at `$XDG_STATE_HOME/schemaforge/token` (fallback `~/.local/state/schemaforge/token`) so subsequent `entity` commands pick it up automatically.

```bash
schemaforge login --server https://forge.agency.gov -u alice    # prompts for password
printf '%s' "$PW" | schemaforge login --server https://forge.agency.gov -u alice --password-stdin
schemaforge login --server https://forge.agency.gov -u alice --print-token   # also echo token to stdout
schemaforge login --server https://forge.agency.gov -u alice --no-save       # don't cache
```

| Flag | Purpose |
|------|---------|
| `--server <url>` | Base URL of the running instance (env `SCHEMAFORGE_SERVER`, default `http://127.0.0.1:3000`) |
| `-u, --username <name>` | Username to authenticate |
| `--password-stdin` | Read the password from stdin instead of prompting (never a `--password` flag — argv leaks via `ps`/history) |
| `--print-token` | Also write the raw token to stdout (pipeable) |
| `--no-save` | Skip the token cache; print only |

Reports `expires_at` and granted `roles` to stderr on success.

### `schemaforge entity <verb>`

Call the entity REST API on a **running** instance over HTTP. Unlike `apply` / `inspect` / `migrate` — which connect straight to the database via `--db-url` — this group speaks the REST API against a deployed server and authenticates with a Bearer token. It is the ergonomic replacement for hand-built `curl`: typed input, filter/sort flags that mirror the wire grammar, and stable exit codes.

```bash
# create — typed --set (numbers/bools coerced), := for raw JSON, relations are ID strings
schemaforge entity create Contact --server https://forge.agency.gov \
  --set first_name=Alice --set last_name=Stone --set active=true \
  --set 'tags:=["vip"]' --set agency=entity_01abc...

# list — raw field__op=value operands or convenience flags; sort/fields/pagination
schemaforge entity list Contact --eq status=active --gte age=18 --sort -created_at --limit 25
schemaforge entity list Contact --fields id,first_name,email --format json | jq '.entities[]'

# get one
schemaforge entity get Contact entity_01abc...

# patch — PARTIAL merge (only the fields you pass change; no fetch-merge-update dance)
schemaforge entity patch Contact entity_01abc... --set status=inactive

# replace — PUT (full entity; every required field must be present)
schemaforge entity replace Contact entity_01abc... --data @contact-full.json

# delete (confirm in scripts with --yes)
schemaforge entity delete Contact entity_01abc... --yes

# query — advanced JSON filter (and/or/not, nested), POST .../entities/query
schemaforge entity query Contact --filter '{"op":"contains","field":"name","value":"Ali"}'
```

Verb → HTTP: `list`=GET `…/entities`, `query`=POST `…/entities/query`, `get`=GET `…/entities/{id}`, `create`=POST `…/entities` (201), `replace`=PUT, `patch`=PATCH, `delete`=DELETE (204). The request URL is `<--server>/api/<--api-version>/forge/...` (default api-version `v1`).

**Connection flags** (shared by every `entity` verb and `login`; these are *not* the global `--db-url` family):

| Flag | Env | Default | Purpose |
|------|-----|---------|---------|
| `--server <url>` | `SCHEMAFORGE_SERVER` | `http://127.0.0.1:3000` | Base URL of the running instance |
| `--api-version <v>` | — | `v1` | API version path segment |
| `--token-file <path>` | — | — | Read the Bearer token from a file |
| `--token-stdin` | — | — | Read the Bearer token from stdin (highest precedence) |
| `--ca-cert <path>` | — | — | PEM CA certificate for a private-PKI server cert |
| `--insecure` | — | off | Skip TLS verification; warns every call. Never in production |
| `--timeout <secs>` | — | `30` | Per-request timeout |

**Token sources, ranked** (there is no `--token` flag — argv leaks via `ps`/history): `--token-stdin` > `--token-file` > `SCHEMAFORGE_TOKEN` env > cached `login` token. Get a token with `schemaforge login` (credential flow) or `schemaforge token generate` (offline). The token is never logged, even at `-vvv`.

**Typed input** (`create` / `replace` / `patch`): `--set field=value` coerces numbers and `true`/`false`; strings and identifiers with leading zeros stay strings (round-trip guard). Use `--set 'field:=json'` for arrays/objects/relation lists, or `--data @file.json` / `--data -` / `--data '{...}'` for a full body (a bare field map is auto-wrapped in `{"fields":{...}}`). `--set` overlays `--data`.

**Filtering / output**: raw `field__op=value` operands pass straight through; convenience flags `--eq/--ne/--gt/--gte/--lt/--lte/--contains/--startswith f=v` and `--in f=a,b,c`; `--sort -f,g` or `f:desc`; `--fields`, `--limit`, `--offset`, `--no-count`, `--no-resolve`. `--format json` prints the raw server response (pipeable); `--dry-run` previews method+URL+body without sending. See [query-api-reference.md](query-api-reference.md) for the wire-level filter grammar these flags map onto.

**Exit codes** (branchable in scripts): `0` ok; `1` general (404, 409, unconfirmed delete); `2` invalid input (422 validation, missing token); `10` connection (refused/DNS/TLS/timeout); `12` server (5xx); `14` auth failed (401 — run `login`); `15` forbidden (403).

**Config**: pin connection defaults in a CLI-only `[schema_forge.client]` section of `config.toml` (`server`, `token_file`, `ca_cert`, `timeout_secs`); precedence is flags > env > config > defaults. The running server ignores this section. Full reference: `docs/entity-cli-reference.md` in the schemaforge repo.

### `schemaforge entity export <SCHEMA>`

Bulk-export entities to a file (`POST .../entities/export`). An export is the `entity query` path with no page limit, materialized to a file — gated separately from read: the schema must declare `@export` and each column must be `@exportable`, so the file is never wider than what the caller could already read.

```bash
# small CSV streams inline to a file
schemaforge entity export Contact --eq status=active --fields first_name,last_name -o contacts.csv

# lossless NDJSON to stdout, piped to jq
schemaforge entity export Contact --format ndjson | jq '.'

# advanced JSON filter (same grammar as `entity query --filter`)
schemaforge entity export Contact --filter '{"op":"contains","field":"name","value":"Ali"}' -o out.csv

# xlsx / zip (and --async, or an over-cap result) run as a background job;
# --out a directory to land the artifact under the server-suggested filename
schemaforge entity export Contact --format xlsx --async -o ./exports/
```

| Flag | Default | Purpose |
|------|---------|---------|
| `--format <csv\|ndjson\|xlsx\|zip>` | `csv` | Deliverable format. `csv`/`ndjson` stream inline when within the schema's `@export(max_rows)` cap; `xlsx`/`zip` always run as a job. Must be in the schema's `@export(formats: [...])`. |
| `--filter <JSON\|@FILE\|->` | — | JSON filter body — a `{...}` literal, `@file.json`, or `-` for stdin. Same grammar as `entity query`. The `--eq/--ne/--gt/...` convenience flags also apply. |
| `--fields <a,b,c>` | all exportable | Column subset (comma-separated). Intersected server-side with `@exportable` ∩ readable; only ever narrows. |
| `-o, --out <PATH\|->` | stdout | Write the artifact here. `-` or omitted writes to stdout; a directory or trailing-`/` path writes under the server-suggested filename (parsed from `Content-Disposition`). |
| `--async` | off | Force the async-job path even for a small CSV/NDJSON export, then poll the job to completion and download. Implied for `xlsx`/`zip` and whenever the result exceeds the row cap. |
| `--poll-interval <SECS>` | `2` | Seconds between job-status polls on the `--async` path. |
| `--poll-timeout <SECS>` | `300` | Give up polling after this many seconds (the job keeps running server-side; re-poll later). `0` waits indefinitely. |

The `--async` path polls `GET .../exports/{job_id}` to `complete`, then downloads the time-limited presigned artifact URL **without** a Bearer token (it is self-authorizing and points at the object store, not the API). Connection flags (`--server`, `--token-*`, …) and exit codes are shared with the other `entity` verbs: `15` on a `403` export-denied, `2` on `422`. See [rest-api-reference.md](rest-api-reference.md) and [export.md](export.md).

### `schema-forge export openapi [PATHS...]`

Export OpenAPI specification from schema files.

```
schema-forge export openapi                              # stdout
schema-forge export openapi -o api.json                  # write to file
schema-forge export openapi --base-path /api             # custom base path
schema-forge export openapi --spec-version 3.1.0         # OpenAPI version
```

### `schema-forge policies list [SCHEMA]`

List generated Cedar authorization policies.

```
schema-forge policies list              # all schemas
schema-forge policies list Contact      # specific schema
```

### `schema-forge policies regenerate [SCHEMA]`

Regenerate Cedar policy templates from schema `@access` annotations.

```
schema-forge policies regenerate                          # all schemas
schema-forge policies regenerate Contact                  # specific schema
schema-forge policies regenerate -o policies/generated/   # output directory
schema-forge policies regenerate --force                  # overwrite existing
```

### `schema-forge policies validate [SCHEMA_PATHS...]`

Compile the full Cedar bundle (generated schema-forge policies + every `*.cedar` file under `--custom-dir`) into a `PolicyStore` and run **strict-mode** validation. Exits non-zero on any error so CI / pre-deploy hooks can gate releases on a passing bundle. This is the same compilation path the runtime uses; passing here means `serve` will mount the store cleanly.

```
schema-forge policies validate                                          # default: schemas/
schema-forge policies validate src/schemas/
schema-forge policies validate --custom-dir policies/custom/            # merge hand-written .cedar files
schema-forge policies validate --role-ranks policies/role_ranks.toml    # default path; missing = empty hierarchy
schema-forge policies validate --format json                            # machine-readable error report
```

Use this in CI before merging schema changes — strict-mode failures here are the same ones the runtime would refuse to hot-swap on `apply`, so catching them at PR time avoids deploys that would roll back automatically.

### `schema-forge bootstrap-admin`

Seed the initial `platform_admin` user against the configured backend. Idempotent: refuses to run when other users already exist so provisioning pipelines (init containers, ansible playbooks, DR runbooks) can't accidentally double-seed. Reads backend connection settings from the same precedence chain as `serve` (CLI flag → env → config.toml).

```
schema-forge bootstrap-admin --password "$ADMIN_PASSWORD"
schema-forge bootstrap-admin --username root --password "$ADMIN_PASSWORD" --display-name "Root Operator"
SCHEMA_FORGE_BOOTSTRAP_ADMIN_PASSWORD="$ADMIN_PASSWORD" schema-forge bootstrap-admin
```

| Flag | Env Var | Default |
|------|---------|---------|
| `--username` | `SCHEMA_FORGE_BOOTSTRAP_ADMIN_USERNAME` | `admin` |
| `--password` | `SCHEMA_FORGE_BOOTSTRAP_ADMIN_PASSWORD` | (required) |
| `--display-name` | `SCHEMA_FORGE_BOOTSTRAP_ADMIN_DISPLAY_NAME` | `Administrator` |

The created row lands in the system `User` table (the same canonical store `EntityAuthStore` reads); the password is argon2-hashed into the `@hidden` `password_hash` field. Never prompted interactively — operators run this from non-interactive provisioning contexts.

### `schema-forge hooks generate`

Scaffold a gRPC hook service (an `acton-service` Rust project) from schemas annotated with `@hook(...)`. You never hand-write the protobufs — `build.rs` compiles them and emits a `FileDescriptorSet` that SchemaForge loads at startup.

```
schema-forge hooks generate --all --schema-dir schemas --out-dir hooks-service
schema-forge hooks generate --schema Translation --out-dir translation-hooks
schema-forge hooks generate --all --regenerate     # one-shot: rewrite every Preserve file
```

- `--all` — combined project for every schema with hooks (recommended topology).
- `--schema <Name>` — per-schema project for independently-deployed services.
- `--regenerate` — full-rewrite escape hatch. Clobbers `main.rs`, `build.rs`, `src/hooks/mod.rs`, and `src/hooks/<schema>.rs` back to the default scaffold. Subsumes the legacy `--force-user-files` flag. Use only when you want to abandon customizations.

**Default mode is additive.** Adding a new `@hook`-annotated schema and re-running `schema-forge hooks generate --all` (with no flags) will splice the new schema into `src/main.rs` and `src/hooks/mod.rs` between stable `SCHEMAFORGE_HOOKS_*` marker comments, leaving every byte outside the markers untouched. Custom module imports (`mod api; mod guard;`), env-var validation, per-service constructor wiring, and hand-written `pub mod` lines all survive regen. Legacy projects (generated before the markers existed) are transparently upgraded to the marker-bounded layout on the first run under the new CLI — no user action required.

Per-schema Owned artifacts (proto files and `.prompt.md`) are always rewritten on every run regardless of flags — those are schema-derived and safe to regenerate.

Layout produced:

- **Preserve** — `Cargo.toml`, `build.rs`, `src/main.rs`, `src/hooks/mod.rs`, `src/hooks/<schema>.rs`. Written once, then user-owned. `main.rs` and `mod.rs` carry insertion markers so the generator can splice new schemas in without clobbering them. Keep the markers in place — remove them only if you want to opt out of additive updates.
- **Owned** — `proto/<schema>_hooks.proto`, `src/hooks/<schema>/<event>.prompt.md`. Rewritten on every run.

### `schema-forge hooks list`

Enumerate every `@hook` annotation across a schema directory.

```
schema-forge hooks list --schema-dir schemas
```

### `schema-forge hooks diff`

Compare two schema directories and report hook-level additions, removals, and intent changes. Use in CI to gate schema PRs on whether downstream hook services need regeneration.

```
schema-forge hooks diff schemas/old schemas/new
```

Markers: `+` added hook, `-` removed hook, `~` intent changed. The diff engine emits three migration steps — `AddHook`, `RemoveHook`, `ChangeHookIntent` — all **metadata-only** (no on-disk migration). The operator action is regenerating and redeploying the hook service so its proto matches the new schema shape.

### `schemaforge sign <PATHS>...`

Produce per-file `.sig` files and a signed `schemas.manifest.toml` for each path. The manifest pins SHA-256 hashes and enumerates the expected file list; the sibling `.sig` defeats add/remove attacks the per-file scheme can't see alone.

```bash
schemaforge sign schemas/ --ed25519-generate --print-pubkey
schemaforge sign schemas/ --ed25519-key keys/sf-signing.key
schemaforge sign schemas/ --ssh-key ~/.ssh/id_ed25519 --ssh-principal roland@govcraft.ai
schemaforge sign schemas/ --keyless                              # CI: rides ambient OIDC token
schemaforge sign schemas/ --keyless --cosign-bin /opt/cosign/cosign
```

| Flag | Meaning |
|------|---------|
| `--ed25519-generate` | Generate a fresh ed25519 keypair, sign with it, print the private-key path |
| `--ed25519-key <PATH>` | Sign with an existing ed25519 PKCS#8 private key |
| `--ssh-key <PATH>` | Sign with an existing OpenSSH private key (SSHSIG format, equivalent to `ssh-keygen -Y sign`) |
| `--ssh-principal <ID>` | Principal label printed in the allowed-signers advisory output |
| `--keyless` | Shell out to `cosign sign-blob --yes --bundle`; on-disk `.sig` becomes a Sigstore Bundle JSON |
| `--cosign-bin <PATH>` | Override the `cosign` binary location (default `cosign` on `$PATH`) |
| `--print-pubkey` | Print the `[[schema_forge.signing.trusted_signers]]` TOML block matching the chosen scheme |

`.sig` files for `cosign-keyless` are Sigstore Bundles (`application/vnd.dev.sigstore.bundle.v0.3+json`), not raw signature bytes — bundles embed the Rekor inclusion proof, so the historical signing time survives Fulcio cert expiry.

### `schemaforge verify <PATHS>...`

Standalone verifier — touches no database. Exit 0 = clean; exit 13 = at least one failure. Suitable as a pre-merge CI gate, separate from any `apply` / `serve` invocation.

```bash
schemaforge verify schemas/
schemaforge verify --trust-policy /etc/schemaforge/trust.toml schemas/
```

Runs the same four checks as the implicit pre-parse verifier: manifest signature → directory ↔ manifest cross-check → per-file signature → SHA-256 hash match. Failures log structured errors with file path, signer kind, and reason.

### `schemaforge trust-bundle refresh`

Perform a full Sigstore TUF fetch on a connected host and serialize the resulting `TrustedRoot` to disk as pretty-printed JSON. Used to prepare airgap / SCIF deployments — the operator copies the file across the airgap and points `[schema_forge.signing] trust_root_bundle` at it.

```bash
schemaforge trust-bundle refresh --output trust_root.json
schemaforge trust-bundle refresh --output trust_root.json --instance staging
schemaforge trust-bundle refresh --output /etc/schemaforge/trust_root.json --force
```

| Flag | Meaning |
|------|---------|
| `--output <PATH>` | Destination path for the trust root JSON (required) |
| `--instance <NAME>` | One of `public-good` (default), `staging`, `github` |
| `--force` | Overwrite an existing file at `--output` |

### `schemaforge trust-bundle inspect <PATH>`

Prints a one-line `fulcio_certs=N rekor_keys=M tsa_certs=K` summary; confirms the bundle is non-empty and parseable before deployment.

```bash
schemaforge trust-bundle inspect /etc/schemaforge/trust_root.json
```

### `schema-forge token init-key`

Generate a 32-byte PASETO V4 symmetric key file.

```
schema-forge token init-key                       # default: ./keys/paseto.key
schema-forge token init-key --output /path/to/key
```

### `schema-forge token generate`

Generate a PASETO token with specified claims.

```
schema-forge token generate --key ./keys/paseto.key --sub "user:admin" --roles platform_admin
schema-forge token generate --key ./keys/paseto.key --sub "user:jane" --roles sales,member --lifetime 7200
schema-forge token generate --key ./keys/paseto.key --sub "user:admin" --roles platform_admin --tenant-chain '[{"schema":"Organization","entity_id":"org-1"}]'
```

Use `platform_admin` for tokens that need to manage users or hit the file scan-complete endpoint. `admin` (or any other string) is just an application-defined role for use in `@access(...)` annotations and carries no platform-wide privileges.

| Flag | Default | Purpose |
|------|---------|---------|
| `--key <PATH>` | `./keys/paseto.key` | Path to symmetric key file |
| `--sub <SUBJECT>` | (required) | Subject claim, use `user:<id>` format |
| `--roles <ROLES>` | — | Comma-separated roles |
| `--lifetime <SECS>` | 3600 | Token lifetime in seconds |
| `--issuer <ISSUER>` | `schema-forge` | Issuer claim |
| `--tenant-chain <JSON>` | — | Tenant scope as JSON array |

### `schema-forge completions <SHELL>`

Generate shell completion scripts.

```
schema-forge completions bash > ~/.bash_completion.d/schema-forge
schema-forge completions zsh > ~/.zfunc/_schema-forge
schema-forge completions fish > ~/.config/fish/completions/schema-forge.fish
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.
