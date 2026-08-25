# Entity CLI Reference

**Drive the entity REST API from the CLI — typed, authenticated, scriptable.**

Every other remote command — `apply`, `inspect`, `migrate` — reaches the
database directly over a `--db-url`. `entity` and `login` are different: they
speak **HTTP to a running server**, carrying a Bearer PASETO token, and never
touch the database. That transport split is the whole point of this reference.
If you have been hand-building `curl` calls and minting offline tokens to script
a deployed instance, these commands replace that — with typed input, real output
formats, and stable exit codes you can branch on.

## Quickstart

Log in once, then run any verb. The token is cached and picked up automatically
on every subsequent call.

```bash
schemaforge login --server https://forge.agency.gov -u alice
# Password: ********
# ok logged in — expires_at 2026-05-24T18:00:00Z, roles [admin]

schemaforge entity create Contact --server https://forge.agency.gov \
  --set first_name=Alice --set last_name=Stone --set 'tags:=["vip"]'
# ok created 01J...

schemaforge entity list Contact --server https://forge.agency.gov --eq status=active
# (aligned table)
# 12 of 42 entities
```

---

## Table of Contents

1. [Transport: a Running Server, Not the Database](#1-transport-a-running-server-not-the-database)
2. [Authenticate Once with `login`](#2-authenticate-once-with-login)
3. [Token Sources (Ranked)](#3-token-sources-ranked)
4. [Connection Flags](#4-connection-flags)
5. [Typing Fields with `--set`](#5-typing-fields-with---set)
6. [Filtering and Sorting](#6-filtering-and-sorting)
7. [Output Formats](#7-output-formats)
8. [Exit Codes](#8-exit-codes)
9. [TLS](#9-tls)
10. [Configuration: `[schema_forge.client]`](#10-configuration-schema_forgeclient)
11. [File Fields: `entity file upload` / `download`](#11-file-fields-entity-file-upload--download)
12. [Examples](#12-examples)

---

## 1. Transport: a Running Server, Not the Database

`entity` and `login` are HTTP clients: they build a URL, attach a Bearer token,
and send a request to a deployed instance. There is no `--db-url` here — the
database is the server's concern, not the client's.

The request URL is composed mechanically:

```
<--server> + /api/<--api-version>/forge + <route>
```

With the defaults (`--server http://127.0.0.1:3000`, `--api-version v1`), the
base is `http://127.0.0.1:3000/api/v1/forge`. Pointed at a deployment with
`--server https://forge.agency.gov`, it becomes
`https://forge.agency.gov/api/v1/forge`.

Each verb maps to one method, one route, and one success status:

| Verb      | Method   | Route (under `{base}`)                  | Success |
|-----------|----------|-----------------------------------------|---------|
| `list`    | `GET`    | `/schemas/{schema}/entities`            | `200`   |
| `query`   | `POST`   | `/schemas/{schema}/entities/query`      | `200`   |
| `get`     | `GET`    | `/schemas/{schema}/entities/{id}`       | `200`   |
| `create`  | `POST`   | `/schemas/{schema}/entities`            | `201`   |
| `replace` | `PUT`    | `/schemas/{schema}/entities/{id}`       | `200`   |
| `patch`   | `PATCH`  | `/schemas/{schema}/entities/{id}`       | `200`   |
| `delete`  | `DELETE` | `/schemas/{schema}/entities/{id}`       | `204`   |
| `export`  | `POST`   | `/schemas/{schema}/entities/export`     | `200`/`202` |
| `login`   | `POST`   | `/auth/login`                           | `200`   |

`replace` sends a `PUT` (full entity — every required field must be present).
`patch` sends a `PATCH` (partial merge — only the fields you supply change).
`export` sends a `POST` to the bulk-export endpoint: a small streamable result
returns `200` with the file inline; an `xlsx`/`zip`, `--async`, or over-cap
result returns `202` with a job id that `--async` polls
(`GET /schemas/{schema}/exports/{job_id}`) to completion. See §11 and the skill's
`export.md` for the full feature.

---

## 2. Authenticate Once with `login`

`login` exchanges a username and password for a token, then caches it. Every
later `entity` call finds that cached token on its own — you authenticate once
per session, not once per command.

```
schemaforge login --server <url> -u <username> [--password-stdin] [--print-token] [--no-save]
```

`login` posts `{username, password}` to `{base}/auth/login`, reads back
`{token, expires_at, roles}`, and reports `expires_at` and `roles` to stderr so
you can confirm what you got.

**Supplying the password.** Interactive use prompts for the password (no echo);
for scripts, pipe it on stdin with `--password-stdin`. There is deliberately no
`--password` flag — a password on argv leaks through `ps` and shell history.

```bash
# Interactive
schemaforge login --server https://forge.agency.gov -u alice

# Non-interactive
printf '%s' "$FORGE_PASSWORD" | schemaforge login \
  --server https://forge.agency.gov -u alice --password-stdin
```

**Caching.** By default the token is written `0600` to
`$XDG_STATE_HOME/schemaforge/token` (falling back to
`~/.local/state/schemaforge/token`). Subsequent `entity` calls read it from
there as the lowest-precedence source (see [§3](#3-token-sources-ranked)).

| Flag             | Effect                                                              |
|------------------|--------------------------------------------------------------------|
| `--print-token`  | Also write the raw token to stdout (capture it for another step)   |
| `--no-save`      | Skip the cache; the token lives only as long as your capture of it |

`login` acquires a token from credentials. This is distinct from
`schemaforge token generate`, which mints a token offline from a PASETO signing
key with no server round-trip — use `token generate` for CI identities and
service accounts, `login` for an interactive operator at a deployed instance.

---

## 3. Token Sources (Ranked)

The Bearer token is supplied through one of four ranked sources. The CLI checks
them in order and uses the first that yields a token:

1. `--token-stdin` — read the token from stdin (highest precedence)
2. `--token-file <path>` — read it from a file
3. `SCHEMAFORGE_TOKEN` — read it from the environment
4. cached `login` token — `$XDG_STATE_HOME/schemaforge/token` (lowest)

There is no `--token` flag. A token passed as an argument leaks through `ps` and
shell history, so the CLI never accepts one on argv; all four sources above keep
the secret off the command line.

The token is never logged, even at `-vvv` — it is redacted from all debug output.

```bash
# CI: token arrives on stdin, never touches the process table
printf '%s' "$FORGE_TOKEN" | schemaforge entity list Contact --token-stdin

# Token in a 0600 file managed by your secret store
schemaforge entity list Contact --token-file /run/secrets/forge-token
```

---

## 4. Connection Flags

Every `entity` verb and `login` share the same connection flags. Defaults point
at a local instance, so a development loop needs none of them.

| Flag                | Env                  | Default                 | Purpose                                                          |
|---------------------|----------------------|-------------------------|------------------------------------------------------------------|
| `--server <url>`    | `SCHEMAFORGE_SERVER` | `http://127.0.0.1:3000` | Base URL of the running instance; the versioned path is appended |
| `--api-version <v>` | —                    | `v1`                    | API version path segment                                         |
| `--token-file <path>` | —                  | —                       | Read the Bearer token from a file (see [§3](#3-token-sources-ranked)) |
| `--token-stdin`     | —                    | —                       | Read the Bearer token from stdin (highest precedence)            |
| `--ca-cert <path>`  | —                    | —                       | PEM CA certificate for a private-PKI server certificate          |
| `--insecure`        | —                    | off                     | Skip TLS verification; warns on every call. Never in production. |
| `--timeout <secs>`  | —                    | `30`                    | Per-request timeout                                              |

`login` ignores the token-source flags (`--token-file`, `--token-stdin`) — it is
acquiring a token, not presenting one.

---

## 5. Typing Fields with `--set`

The server is strict about types: a field declared `integer` rejects the string
`"25"`. `--set` types your values so they arrive as the JSON the server expects.
It applies to `create`, `replace`, and `patch`.

**Typed coercion.** `--set field=value` (repeatable) infers the JSON type:

```bash
--set max_seats=25        # → JSON number 25
--set active=true         # → JSON boolean true
--set name="Alice Stone"  # → JSON string
```

**Round-trip guard.** Coercion is suppressed when turning a value into a number
would lose information — so identifiers stay strings:

| Input             | Becomes         | Why                                                  |
|-------------------|-----------------|------------------------------------------------------|
| `zip=01234`       | `"01234"`       | Leading zero would not survive a round-trip          |
| `account=900000000000000001` | `"900..."` | Large integer-like IDs stay strings                  |
| `price=9.99`      | `9.99` (float)  | Round-trips exactly                                  |
| `price=2.70`      | `"2.70"`        | Trailing zero would not survive a round-trip         |

A value is coerced only if parsing and re-printing it yields the identical text;
anything that would not round-trip stays a string.

**Raw JSON with `:=`.** For arrays, objects, and relation references, use the
`:=` form (httpie's escape) to supply explicit JSON:

```bash
--set 'tags:=["vip","gov"]'        # array
--set 'owner:="user_01H..."'       # string, forced
--set 'address:={"city":"Reston"}' # object
```

**Full bodies with `--data`.** Supply a complete body from a file, stdin, or a
literal:

```bash
--data @contact.json
--data -            # read from stdin
--data '{"fields": {"first_name": "Alice"}}'
```

A bare field map is auto-wrapped into `{"fields": {...}}`; a body that is already
wrapped is accepted as-is. `--set` overlays `--data`, so you can load a base
body from a file and override individual fields on the command line.

---

## 6. Filtering and Sorting

`list` accepts filters and sorts in two compatible forms. Both compile to the
same wire grammar; pick whichever reads better in your shell.

**Raw operands.** A `field__op=value` operand passes straight through to the
query string, 1:1 with the server grammar. A bare `field=value` is equality:

```bash
schemaforge entity list Contact age__gte=18 age__lt=65 status=active
```

**Convenience flags.** Each is repeatable and expands to the matching operator:

| Flag           | Operator              | Example                         |
|----------------|-----------------------|---------------------------------|
| `--eq`         | equals                | `--eq status=active`            |
| `--ne`         | not equals            | `--ne status=archived`          |
| `--gt`         | greater than          | `--gt age=25`                   |
| `--gte`        | greater than or equal | `--gte age=18`                  |
| `--lt`         | less than             | `--lt age=65`                   |
| `--lte`        | less than or equal    | `--lte score=100`               |
| `--contains`   | substring match       | `--contains name=smith`         |
| `--startswith` | prefix match          | `--startswith email=admin`      |
| `--in`         | set membership        | `--in status=active,pending`    |

**Sorting and shaping.**

| Flag          | Effect                                                          |
|---------------|----------------------------------------------------------------|
| `--sort <s>`  | `--sort -age,name` (leading `-` = desc) or `--sort age:desc`    |
| `--fields <l>`| CSV projection — return only these fields                      |
| `--limit <N>` | Maximum results to return                                      |
| `--offset <N>`| Number of results to skip                                     |
| `--no-count`  | Set `count=false` (skip the total-count computation)           |
| `--no-resolve`| Set `resolve=false` (skip relation display-name resolution)    |

For boolean logic that query strings cannot express, use
`schemaforge entity query` with `--filter` (`@file`, `-`, or a literal JSON
object). The wire-level operator set, logical combinators (`and`/`or`/`not`),
dotted field paths, and type-coercion rules live in
[`docs/query-api-reference.md`](query-api-reference.md); this section covers
only the CLI surface that maps onto them.

---

## 7. Output Formats

Data goes to **stdout**; status and diagnostics go to **stderr**. That split is
what makes the CLI scriptable: pipe stdout to `jq` or `cut`, and the `ok created`
confirmations never contaminate your data stream.

**`--format human`** (default). `list` and `query` render an aligned table with
a footer:

```
12 of 42 entities          # "N of M" when count is available
12 entities                # "N entities" under --no-resolve / --no-count
```

`get` renders key/value pairs. Writes confirm to stderr:

```
ok created 01J...
ok updated 01J...
ok deleted Contact/01J...
```

**`--format json`.** The raw server response (`EntityResponse` /
`ListEntitiesResponse`) is written to stdout unmodified — directly pipeable into
`jq`.

**`--format plain`.** Tab-separated rows, for `cut` and `awk`.

**`--dry-run`** (on `create`, `replace`, `patch`, `delete`). Prints the resolved
method, URL, and JSON body — then stops. Nothing is sent. Use it to confirm what
a write would do before committing to it.

---

## 8. Exit Codes

Every failure maps to a stable exit code. Branch on these in scripts instead of
grepping output.

| Code | Meaning                          | Maps from                                  | Tip                                                |
|------|----------------------------------|--------------------------------------------|----------------------------------------------------|
| `0`  | Success                          | `2xx`                                       | —                                                  |
| `1`  | General                          | `404`, `409` conflict, unconfirmed delete   | Check the id; pass `--yes` for non-interactive delete |
| `2`  | Invalid arguments / input        | `422` validation (grouped `details[]`), missing token | Read the grouped field errors; run `login` if no token |
| `10` | Connection                       | refused, DNS, TLS handshake, timeout        | Check `--server` and that the instance is up       |
| `12` | Server                           | `5xx`                                        | Server-side fault; check the instance's logs       |
| `14` | Authentication failed            | `401`                                        | Run `schemaforge login`, or check the token        |
| `15` | Forbidden                        | `403`                                        | Authenticated, but the role lacks this permission  |

The distinct codes let a script tell "your input is wrong" (`2`, carrying the
server's grouped `422` `details[]`) from "the server is broken" (`12`) from "you
are not allowed" (`15`).

The wider CLI also uses `3` (parse), `11` (migration), and `13` (verification)
for non-`entity` commands.

---

## 9. TLS

TLS verification is strict by default. A server presenting a certificate from a
private PKI is verified against the CA you supply:

```bash
schemaforge entity list Contact \
  --server https://forge.agency.gov \
  --ca-cert /etc/pki/agency-root.pem
```

`--insecure` skips verification entirely and warns loudly on every call. It
exists for throwaway local testing; never use it in production.

The TLS stack is FIPS-consistent: `reqwest` runs through `rustls` backed by the
process-default `aws-lc-rs` crypto provider — the same provider every other
TLS-using subsystem installs at startup.

---

## 10. Configuration: `[schema_forge.client]`

Pin connection defaults in `config.toml` so day-to-day commands need no flags.
This section is **client-only** — a running server ignores it entirely.

Precedence is **flags > environment > config > built-in defaults**: a flag
always wins over config, and config always wins over the shipped defaults.

```toml
[schema_forge.client]
server = "https://forge.agency.gov"
token_file = "~/.config/schemaforge/token"
ca_cert = "/etc/pki/agency-root.pem"
timeout_secs = 30
```

With this in place, `schemaforge entity list Contact` talks to the agency
instance over the agency PKI with no flags at all; add `--server …` on any call
to override it for one invocation.

---

## 11. File Fields: `entity file upload` / `download`

A `file` field is an **S3-backed attachment**, not an inline value. The runtime
never proxies upload bytes — the client PUTs directly to storage through a
presigned URL the runtime mints. These two verbs wrap that handshake so you do
not have to script it by hand.

```bash
# Upload a local file to the `contract` field of one Document.
schemaforge entity file upload Document 01J... contract ./proposal.pdf
# ok uploaded contract (available)
# status: available
# key:    tenant_a/Document/01J.../contract/01HX.../proposal.pdf

# Download it back (defaults to a sanitized basename from the response).
schemaforge entity file download Document 01J... contract
# ok wrote 248213 bytes to proposal.pdf

# Stream to a chosen path, or to stdout with `-`.
schemaforge entity file download Document 01J... contract --out /tmp/c.pdf
schemaforge entity file download Document 01J... contract --out - | less
```

### The handshake `upload` wraps

`upload` performs three round-trips so the bytes never touch the runtime:

1. **Mint** — `POST .../fields/{field}/upload-url` asserts the filename, MIME,
   and size. The server checks the MIME against the field's allowlist and the
   size against `max_size`, runs any `before_upload` hook (which may abort), and
   returns a presigned URL plus the exact headers to replay.
2. **PUT** — the file's bytes stream **directly to storage** at that URL,
   carrying the returned headers verbatim and **no** Bearer token (the presigned
   URL is self-authenticating). A streaming read keeps memory flat regardless of
   file size.
3. **Confirm** — `POST .../fields/{field}/confirm-upload` hands back the object
   key and a SHA-256 checksum (always computed; stored for forensics). The
   server re-validates size and MIME from storage and records the attachment.

### Flags

| Flag | Verb | Meaning |
| --- | --- | --- |
| `--mime TYPE` | `upload` | Assert the MIME type. Overrides extension detection. If neither an override nor a detected type is available, the upload errors and asks for `--mime` — the asserted type must match the field's allowlist, so it is never guessed as `application/octet-stream`. |
| `--filename NAME` | `upload` | Filename recorded in the object key. Defaults to the path's file name. |
| `--out PATH` | `download` | Destination path. `-` streams to stdout. Absent, the name is derived from the final response URL (last path segment), sanitized to a single safe basename, falling back to the field name. An explicit `--out` is used as given. |

### Scanning status

If the schema declares an `on_scan_complete` hook, `confirm-upload` lands the
attachment in **`scanning`**, not `available` — an external scanner must clear
it first. `upload` prints that status and notes the file is **not downloadable
until it becomes `available`**. `download` of a non-available attachment is
refused with a legible error (e.g. `file not yet available (status: scanning)`);
scripts and agents will hit this routinely while a scan runs, so branch on it.

### Not yet supported

There is no `entity file clear` (or replace) verb: the attachment lifecycle is
deliberately centralized server-side and no route exists to detach or overwrite
an attachment. Re-uploading creates a new object under a new key.

---

## 12. Examples

### Each verb, end to end

```bash
# Authenticate once
schemaforge login --server https://forge.agency.gov -u alice

# Create
schemaforge entity create Contact --server https://forge.agency.gov \
  --set first_name=Alice --set last_name=Stone \
  --set status=active --set 'tags:=["vip"]'
# ok created 01J...

# Get one
schemaforge entity get Contact 01J... --server https://forge.agency.gov

# List with filter and sort
schemaforge entity list Contact --server https://forge.agency.gov \
  --eq status=active --gte age=18 --sort -created_at --limit 25

# Patch (partial)
schemaforge entity patch Contact 01J... --server https://forge.agency.gov \
  --set status=inactive

# Replace (full — every required field present)
schemaforge entity replace Contact 01J... --server https://forge.agency.gov \
  --data @contact-full.json

# Delete (confirm in scripts with --yes)
schemaforge entity delete Contact 01J... --server https://forge.agency.gov --yes

# Export (bulk) — small CSV streams inline to a file
schemaforge entity export Contact --server https://forge.agency.gov \
  --eq status=active --fields first_name,last_name -o contacts.csv

# Export as a background job (xlsx/zip, --async, or over-cap), poll + download
schemaforge entity export Contact --server https://forge.agency.gov \
  --format xlsx --async -o ./exports/
```

`export` requires the schema to declare `@export` and each column to be
`@exportable`; the file is never wider than what the caller could read one record
at a time, and it is gated by a distinct Cedar `Export{Entity}` action (a policy
can permit read-one while forbidding export-many). `--fields` only narrows the
`@exportable` ∩ readable set. A `403` (export-denied) exits `15`; a `422`
(deferred/validation) exits `2`. See the skill's `export.md` for the full model.

### Typed-coercion edge cases

```bash
schemaforge entity create Account \
  --set max_seats=25 \      # number
  --set active=true \       # boolean
  --set zip=01234 \         # stays "01234" (leading zero)
  --set price=9.99 \        # float
  --set list_price=2.70 \   # stays "2.70" (trailing zero)
  --set 'roles:=["admin","auditor"]'   # raw JSON array
```

### Pipe JSON output to jq

```bash
schemaforge entity list Contact --format json --eq status=active \
  | jq -r '.entities[].fields.email'
```

### Preview a write without sending it

```bash
schemaforge entity create Contact \
  --set first_name=Alice --set last_name=Stone --dry-run
# POST https://forge.agency.gov/api/v1/forge/schemas/Contact/entities
# {
#   "fields": { "first_name": "Alice", "last_name": "Stone" }
# }
```

### Non-interactive scripting with exit-code checks

```bash
#!/usr/bin/env bash
set -euo pipefail

# Token from a secret store, on stdin — never on argv
if ! printf '%s' "$FORGE_TOKEN" \
  | schemaforge entity create Contact \
      --server "$FORGE_SERVER" --token-stdin \
      --set first_name=Alice --set last_name=Stone; then
  code=$?
  case $code in
    14) echo "auth failed — token expired; re-issue" >&2 ;;
    15) echo "forbidden — service account lacks create on Contact" >&2 ;;
    2)  echo "validation failed — see details above" >&2 ;;
    10) echo "cannot reach $FORGE_SERVER" >&2 ;;
  esac
  exit "$code"
fi
```
