# SchemaForge Site Guide — React Generator

`schemaforge site generate` scaffolds a Vite + React 19 + Tailwind 4 + shadcn project that talks to a running `schemaforge serve` instance over `/api/v1/forge/*`. This guide is the starting point for anyone who wants to ship a UI on top of their schemas.

## Two ways to get a UI

1. **Bundled ops console — zero config.** A release binary built with the `embedded-console` feature serves the prebuilt [`schemaforge-console`](https://github.com/Govcraft/schemaforge-console) SPA same-origin at **`/console`** — no Node, no build step, no CORS. Just run the server and open the `Console → http://<host>/console` URL it prints, then sign in with the `--admin-user` / `--admin-password` credentials:

   ```bash
   schemaforge serve --schemas schemas --admin-password <pw>
   # → Console → http://127.0.0.1:3000/console
   ```

   The console is schema-generic (it discovers your schemas at runtime via `/api/v1/forge/schemas`), so one bundle works for any schema — there is nothing to regenerate per schema. Pass `--no-console` to run the JSON API only; a binary built without the `embedded-console` feature also serves the API alone.

   Building from source with the console embedded (release CI verifies a signed bundle; locally, point at a built `dist`):

   ```bash
   pnpm --filter @schemaforge/console build      # in the schemaforge-console checkout
   SCHEMAFORGE_CONSOLE_DIST=/path/to/schemaforge-console/apps/console/dist \
     cargo run -p schema-forge-cli --features embedded-console -- \
     serve --schemas schemas --admin-password <pw>
   ```

2. **Generated project — fully customizable.** `schemaforge site generate` scaffolds a Vite + React project with strongly-typed, per-entity pages that you restyle and host yourself. The rest of this guide covers that path.

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

### `/app/*` — codegen'd, strongly-typed pages

One folder per schema under `src/app/pages/<kebab>/`, with `list.tsx`, `detail.tsx`, and `edit.tsx`. These are **Preserve**-mode: scaffolded once, then yours to restyle. Re-running the generator leaves them alone unless `--force-user-files` is passed.

Pages are strongly typed against the generated `src/generated/entity-types.ts` and `src/generated/zod-schemas.ts`. Forms use react-hook-form + zod; lists use TanStack Query with offset-pagination, click-to-sort, and a column-targeted `contains` filter. The sidebar nav is built at runtime from `/api/v1/forge/schemas`, so it lists exactly the (non-`@system`) schemas the signed-in user can read.

> The runtime-dynamic admin console (the former `/admin/*` shell — schema catalog, generic CRUD, and user management) now lives in its own repo, [`schemaforge-console`](https://github.com/Govcraft/schemaforge-console). `schemaforge site generate` no longer produces it.

## File ownership modes

Every scaffolded file is either `Owned` or `Preserve`:

| Mode | Behavior | Typical use |
|------|----------|-------------|
| `Owned` | Regenerated every run. Manual edits are detected as drift and rejected by `--check`. Overwritten by `schemaforge site generate`. | `src/generated/*`, `src/lib/*`, `src/components/ui/*`, `src/app/pages/**/*.generated.tsx`, `src/main.tsx`, `index.html`, `vite.config.ts`, `tailwind.config.ts`. |
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

Files present in the override directory shadow the binary defaults one-for-one. The loader walks the same relative layout as the bundled templates (e.g. `site-templates/src/app/pages/list.tsx.jinja` overrides `crates/schema-forge-cli/templates/site/src/app/pages/list.tsx.jinja`). Iterate on a `.jinja` file, re-run `schemaforge site generate`, Vite HMR picks up the new `.tsx`. No CLI rebuild needed.

## Auth bootstrap

The first time `schemaforge serve` starts against an empty user store, it seeds an admin using `--admin-user` / `--admin-password` (or `FORGE_ADMIN_USER` / `FORGE_ADMIN_PASSWORD`). Subsequent starts keep the existing store.

The React app's login flow:

1. `POST /api/v1/forge/auth/login` with `{ username, password }`.
2. Response body: `{ token, expires_at, roles }`.
3. Client stores the PASETO token + expiry + roles in `sessionStorage`.
4. A silent refresh is scheduled ~5 minutes before `expires_at` via `POST /auth/refresh`, and the api-client retries any 401 once through the refresh endpoint before redirecting to `/login`.

The `roles` claim is stored alongside the token for role-scoped chrome. Field- and record-level access (`@field_access`, `@access`) is enforced authoritatively server-side: the API omits fields the caller may not read and rejects writes it may not make, so the generated pages only ever render what Cedar already permits.

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
| `relation Many` (stored) | CSV `<Input>` | Comma-separated id list; future work: multi-select combobox. |
| `relation Many` (derived inverse, issue #34) | *rendered as a read-only linked list on the detail page* | The backend rejects writes on derived collections, so the generator skips them on create/edit forms and their zod schemas (issue #35). Reads flow through the standard relation envelope — `__display` values are populated by the backend's inverse-collection pass and the detail template renders them as a linked list. To edit membership, write to the child-side FK. |
| `composite { ... }` | Recursive fieldset | Sub-fields are addressed via dot-paths in react-hook-form. |
| `composite[]`, `text[][]` | `<textarea>` (JSON) | Array-of-composite and nested arrays fall back to a JSON textarea (see #18). |

## Troubleshooting

- **`401` immediately after login** — Vite proxy isn't forwarding the `Authorization` header. Check `VITE_FORGE_UPSTREAM` and that `schemaforge serve` is reachable from the dev machine.
- **Empty sidebar / "No application schemas yet."** — the logged-in user has zero read access on every non-`@system` schema. Add a `@access(read=[...])` annotation or sign in as a user who can read them.
- **Stale generated file after a schema change** — `schemaforge site generate` only rewrites `Owned` files. If you edited one, stash the change, regenerate, then re-apply.
- **`schema-forge site generate --check` fails in CI** — you edited an `Owned` file by hand. Move the edit into a `Preserve` file or override the template via `site-templates/`.

## See also

- [`docs/query-api-reference.md`](query-api-reference.md) — REST query parameter grammar
- [`docs/hooks-reference.md`](hooks-reference.md) — lifecycle hook service scaffolding
