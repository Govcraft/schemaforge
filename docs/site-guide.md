# SchemaForge Site Guide — React Generator

`schemaforge site generate` scaffolds a Vite + React 19 + Tailwind 4 + shadcn project that talks to a running `schemaforge serve` instance over `/api/v1/forge/*`. This guide is the starting point for anyone who wants to ship a UI on top of their schemas.

## Quick start

```bash
# 1. scaffold the project next to your schemas
schemaforge site generate -s schemas -o site

# 2. install deps and start the dev server
cd site
pnpm install
pnpm dev
```

Vite proxies `/api/v1/*` to `http://localhost:3000` by default; set `VITE_FORGE_UPSTREAM` in `.env.local` if `schemaforge serve` runs on a different port. Open `http://localhost:5173`, click "Sign in", and use the credentials printed by `schemaforge serve --admin-user admin --admin-password <pw>`.

## What the generator produces

The output has two independent top-level route trees:

### `/app/*` — codegen'd, strongly-typed pages

One folder per schema under `src/app/pages/<kebab>/`, with `list.tsx`, `detail.tsx`, and `edit.tsx`. These are **Preserve**-mode: scaffolded once, then yours to restyle. Re-running the generator leaves them alone unless `--force-user-files` is passed.

Pages are strongly typed against the generated `src/generated/entity-types.ts` and `src/generated/zod-schemas.ts`. Forms use react-hook-form + zod; lists use TanStack Query with offset-pagination, click-to-sort, and a column-targeted `contains` filter.

### `/admin/*` — runtime-dynamic admin shell

`src/admin/*` is an **Owned** shell that introspects `/api/v1/forge/schemas` at runtime and renders any schema the authenticated user can read. There is no per-entity codegen on the admin side — add a schema to the backend and it shows up in the admin sidebar after a refresh.

`/admin/users` is the user management surface (list, create, change password, delete), gated on the `admin` role.

## File ownership modes

Every scaffolded file is either `Owned` or `Preserve`:

| Mode | Behavior | Typical use |
|------|----------|-------------|
| `Owned` | Regenerated every run. Manual edits are detected as drift and rejected by `--check`. Overwritten by `schemaforge site generate`. | `src/admin/*`, `src/generated/*`, `src/lib/*`, `src/components/ui/*`, `src/main.tsx`, `index.html`, `vite.config.ts`, `tailwind.config.ts`. |
| `Preserve` | Scaffolded once. Subsequent runs leave the file alone. `--force-user-files` re-scaffolds. | `src/app/pages/**/*.tsx`, `src/pages/login.tsx`, `package.json`. |

`--check` mode does a pure in-memory render and exits non-zero if any `Owned` file differs from what's on disk. Use it in CI to catch drift.

## Customizing templates

All `.jinja` templates are baked into the CLI binary, but you can override any of them without rebuilding:

```bash
# auto-detected: ./site-templates beside the current working directory
schemaforge site generate

# or explicit
schemaforge site generate --templates-dir ./my-templates
```

Files present in the override directory shadow the binary defaults one-for-one. The loader walks the same relative layout as the bundled templates (e.g. `site-templates/src/admin/entity-list.tsx.jinja` overrides `crates/schema-forge-cli/templates/site/src/admin/entity-list.tsx.jinja`). Iterate on a `.jinja` file, re-run `schemaforge site generate`, Vite HMR picks up the new `.tsx`. No CLI rebuild needed.

## Auth bootstrap

The first time `schemaforge serve` starts against an empty user store, it seeds an admin using `--admin-user` / `--admin-password` (or `FORGE_ADMIN_USER` / `FORGE_ADMIN_PASSWORD`). Subsequent starts keep the existing store.

The React app's login flow:

1. `POST /api/v1/forge/auth/login` with `{ username, password }`.
2. Response body: `{ token, expires_at, roles }`.
3. Client stores the PASETO token + expiry + roles in `sessionStorage`.
4. A silent refresh is scheduled ~5 minutes before `expires_at` via `POST /auth/refresh`, and the api-client retries any 401 once through the refresh endpoint before redirecting to `/login`.

The `roles` claim drives client-side enforcement of `@field_access(read=[...], write=[...])` annotations: read-denied fields are hidden from list columns and edit forms, write-denied fields are forced to read-only.

## Production builds

```bash
pnpm build          # typechecks and emits static dist/
pnpm preview        # local sanity check of the production build
```

`dist/` is a plain static bundle — drop it behind any reverse proxy that also routes `/api/v1/*` to the `schemaforge serve` instance. The production build does not embed `VITE_FORGE_UPSTREAM`; set it via the shell environment or a `.env.production` file before `pnpm build`.

## Field types reference

| DSL type | React widget | Notes |
|----------|--------------|-------|
| `text` | `<Input>` | `@widget("richtext")` / `@widget("textarea")` upgrade to a multi-line `<textarea>`. |
| `text(max: N)` | `<Input maxLength={N}>` | Max length is also reflected in the zod schema. |
| `rich_text` | `<textarea>` | Rendered verbatim; no editor widget in v1. |
| `integer` / `float` | `<Input type="number">` | Form state is string; handler coerces to number on submit. |
| `boolean` | `<input type="checkbox">` | |
| `datetime` | `<input type="datetime-local">` | Emits `YYYY-MM-DDTHH:MM`; edit handler round-trips to ISO-8601 with timezone before submit. |
| `enum("a", "b")` | `<select>` | Variants are frozen at codegen time; regenerate after schema edits. |
| `json` | `<textarea>` | Form state is a JSON string; edit handler runs `JSON.parse` before submit. |
| `relation One` | `<RelationSelect>` | Combobox that fetches the target schema's entities and labels them by the `@display("...")` annotation. |
| `relation Many` | CSV `<Input>` | Comma-separated id list; future work: multi-select combobox. |
| `composite { ... }` | Recursive fieldset | Sub-fields are addressed via dot-paths in react-hook-form. |
| `composite[]`, `text[][]` | `<textarea>` (JSON) | Array-of-composite and nested arrays fall back to a JSON textarea (see #18). |

## Troubleshooting

- **`401` immediately after login** — Vite proxy isn't forwarding the `Authorization` header. Check `VITE_FORGE_UPSTREAM` and that `schemaforge serve` is reachable from the dev machine.
- **`/admin/*` shows "No schemas visible"** — the logged-in user has zero read access. Either add a `@access(read=[...])` annotation or log in as an admin.
- **Stale generated file after a schema change** — `schemaforge site generate` only rewrites `Owned` files. If you edited one, stash the change, regenerate, then re-apply.
- **`schema-forge site generate --check` fails in CI** — you edited an `Owned` file by hand. Move the edit into a `Preserve` file or override the template via `site-templates/`.

## See also

- [DSL reference](../crates/schema-forge-cli/templates/site/README.md) inside the generated project
- [`docs/query-api-reference.md`](query-api-reference.md) — REST query parameter grammar
- [`docs/hooks-reference.md`](hooks-reference.md) — lifecycle hook service scaffolding
