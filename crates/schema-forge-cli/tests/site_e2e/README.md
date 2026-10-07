# Site end-to-end smoke test

This directory holds the Playwright smoke suite for the generated React site. It is **not** wired into the default `cargo test` / `cargo nextest run` flow because it needs a Node toolchain, a live backend, and a browser.

## Layout

```
site_e2e/
├── README.md              # this file
├── demo.schema            # minimal fixture schema (Company with composite, enum, datetime)
├── playwright/
│   ├── package.json       # pins @playwright/test
│   ├── playwright.config.ts
│   └── tests/
│       ├── smoke.spec.ts  # login → /app create → detail round-trip
│       └── a11y.spec.ts   # axe WCAG 2.1 AA scan of login + /app routes
└── run.sh                 # orchestrator: generate site, boot backend, run spec
```

## Running locally

```bash
# from the repo root
cargo run --locked -p schema-forge-test-runner -- ./crates/schema-forge-cli/tests/site_e2e/run.sh
```

`run.sh` does the following:

1. Creates a scratch tempdir under `target/site-e2e-<pid>/`.
2. Copies `demo.schema` into `$TMP/schemas/`.
3. Runs `cargo run --bin schemaforge -- site generate -s $TMP/schemas -o $TMP/site`.
4. Runs `pnpm install` inside `$TMP/site`.
5. Boots `schemaforge serve` against the test runner's remote SurrealDB on an ephemeral port with a seeded `admin/admin` credential.
6. Starts `pnpm dev` pointed at the ephemeral backend via `VITE_FORGE_UPSTREAM`.
7. Waits for `http://localhost:<vite-port>` to be reachable.
8. Runs `pnpm --filter schemaforge-site-e2e exec playwright test` against the dev server.
9. Tears both processes down on exit.

## Running in CI

The CI component selector calls `.github/workflows/site-e2e.yml` for relevant changes, including:

- `crates/schema-forge-codegen/templates/site/**`
- `crates/schema-forge-cli/src/commands/site.rs`
- `crates/schema-forge-acton/src/routes/**`
- `crates/schema-forge-cli/tests/site_e2e/**`

Generator-only checks build the portable CLI and use a released server pinned by
version and SHA-256 digest. Runtime changes and full validation compile the
candidate server. `GENERATOR_BIN` and `SERVER_BIN` can select these executables
independently for local runs. See [component CI](../../../../docs/component-ci.md)
for the complete selection and release policy.

Playwright artifacts (screenshots, traces) are uploaded on failure.

## Spec coverage

Current specs are intentionally narrow — grow them as the site surface grows:

The runtime-dynamic admin console (the old `/admin/*` shell, schema catalog,
generic CRUD, and user management) moved to the `schemaforge-console` repo, so
these specs cover only the generated `/app` per-entity surface.

- `smoke.spec.ts`:
  1. Visit `/login`, submit `admin` / `admin`, assert redirect away from `/login`.
  2. Visit `/app/company`, click **New Company**, fill the required `name` field, submit, assert redirect to the detail view and the headline renders the saved name.
- `a11y.spec.ts`: axe WCAG 2.1 A/AA + Section 508 scan of `/login`, the `/app/company` list, the `/app/company/new` create form, and a `/app/company/:id` detail page — each must report zero violations.
