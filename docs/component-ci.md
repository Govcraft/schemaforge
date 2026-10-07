# Component CI and release validation

Pull requests and pushes to main validate the changed components and their
consumers. The nightly schedule, manual CI runs, and release tags validate every
component.
Release packaging and signing wait for successful validation of the same tag
commit. The required branch-protection check remains `Required CI`.

## Change selection

`scripts/ci/select_checks.py` compares the pull request merge commit with the
merge base of its base branch. A normal main push compares the pre-push commit
with the new tip, including every commit in the push. Missing, zero, or
non-ancestor push bases fall back to full validation. The verified comparison
base is also passed to metadata validation. Deleted and renamed files participate
in selection.
Unknown executable/source paths, shared interfaces, dependencies, feature flags,
toolchains, and CI policy changes select the complete suite.

| Change | Checks |
| --- | --- |
| Documentation | Metadata, CI policy tests, formatting |
| Package versions and changelog only | Metadata, CI policy tests, formatting |
| Code generation or site templates | Portable tooling and generated-site browser smoke |
| Ordinary CLI command | Portable tooling and server CLI tests without a database engine |
| Runtime route | Portable runtime, PostgreSQL, SurrealDB, browser smoke |
| PostgreSQL backend | PostgreSQL and portable runtime |
| SurrealDB backend | SurrealDB and browser smoke |
| MSSQL backend | SQL Server integration and Windows compilation |
| Shared core, authorization, configuration, dependencies | Complete validation |

Selection is conservative. The selector tests specify the precise rules. A
manifest change can use the metadata path only when its parsed contents differ
solely in known workspace package versions and supported local version references.
A lockfile change must preserve external packages, dependency edges, and all
non-version fields. Dependency or feature changes run full validation. Metadata
validation checks workspace versions against the lockfile and requires a changelog
entry when the CLI version changes.

Use these commands to inspect selection locally:

```sh
python3 scripts/ci/select_checks.py --base origin/main --head HEAD
python3 scripts/ci/select_checks.py --event push --base "$(git rev-parse HEAD^)" --head HEAD
python3 scripts/ci/select_checks.py --head HEAD --full
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
python3 scripts/ci/validate_metadata.py --head HEAD
```

## Compilation boundaries

`schema-forge-config` owns the shared site branding configuration.
`schema-forge-codegen` owns site rendering, templates, and the shared generated-file
manifest, marker, drift-checking, and write primitives. It depends on schema types
and rendering libraries, without depending on the server runtime.

The CLI's `server` feature enables runtime commands and dependencies. Backend
features imply `server`; default builds continue to enable SurrealDB. Release
backend feature selections continue to produce the complete executable.

```sh
cargo nextest run -p schema-forge-codegen -p schema-forge-config
cargo nextest run -p schema-forge-cli --no-default-features
cargo nextest run -p schema-forge-cli --no-default-features --features server,oauth,sse
cargo clippy -p schema-forge-cli --no-default-features --all-targets -- -D warnings
cargo run -p schema-forge-cli --no-default-features -- site generate --help
```

The portable CLI supports initialization, parsing, completions, hook and site
generation, and signing tools. Server and database commands are available in
server builds. The tooling build uses shared configuration types for site branding
and signing while the server retains acton-service's canonical configuration.

## Portable and database runtime tests

Portable handler and authorization tests use a test-only, seeded in-memory fixture
implementing the backend interfaces. It verifies response and authorization behavior;
it does not substitute for real storage concurrency or migration tests.

```sh
cargo nextest run -p schema-forge-acton --no-default-features
cargo clippy -p schema-forge-acton --no-default-features --all-targets -- -D warnings
```

Real SurrealDB-backed runtime integration targets require `test-surrealdb`.
GraphQL, OAuth, and event integration targets also require their respective
extension features. Full validation explicitly enables those features and retains
the PostgreSQL disposable-namespace tests, SQL Server tests, and CEL proofs.

```sh
cargo nextest run -p schema-forge-acton --features test-surrealdb,graphql,oauth,sse
```

Do not replace backend CI with `--all-features`: acton-service's primary database
features are mutually exclusive. Run each backend graph separately.

## Generated-site smoke tests

For generator-only changes, CI builds the candidate tooling CLI and runs the
rendered application against a pinned released SurrealDB server. The release asset
is verified against a committed SHA-256 digest. Runtime, database configuration,
and shared-interface changes use a server compiled from the candidate commit.
Full and release validation always compile the candidate server and use that
same executable to generate the site. A second portable executable adds no
browser coverage in this mode: the portable tooling job tests that compilation
boundary, and generator-only changes use it for browser smoke. Generated Node
files live under the runner's temporary directory rather than Cargo's target
directory, so Rust cache collection does not traverse Node package fixtures.

`GENERATOR_BIN` and `SERVER_BIN` let `tests/site_e2e/run.sh` select these binaries
independently. Without those variables, the script builds and uses the default CLI
for both, preserving the local workflow. The script builds and lints the generated
application before running Chromium smoke tests.

After a validated release, update `SITE_SERVER_VERSION` and `SITE_SERVER_SHA256`
together in `.github/workflows/site-e2e.yml`. Never use a moving `latest` server or
a candidate tooling binary from another commit.

## Workflow ownership

- `ci.yml` selects components, calls validation, and reports `Required CI`.
- `full-validation.yml` runs selected reusable suites and rejects failed or
  unexpectedly skipped selected jobs. Its inputs default to full validation.
- `component-checks.yml` runs portable tooling, runtime, or server CLI tests and lints, plus tooling doctests.
- `postgres-conditional.yml`, `surrealdb-runtime.yml`, and `mssql-integration.yml`
  validate real backend graphs independently.
- `site-e2e.yml` validates generation and browser behavior.
- `cel-kani.yml` validates scalar proofs.
- `release.yml` calls full validation before building and signing the release matrix.

Linux test jobs install a pinned prebuilt nextest through a commit-pinned install
action. Component cache keys keep portable and backend feature graphs separate.
The PostgreSQL storage, runtime, and CLI checks use one feature set with OAuth
and SSE enabled. A parallel job validates the deliberately different
extension-disabled graph. Both jobs must succeed before the PostgreSQL reusable
suite passes. Dependency guards reject embedded SurrealDB in either graph and
reject accidentally enabled extensions in the opt-out graph.

Changes to this CI architecture add CLI build capability and should be included
in the next minor CLI release. This change does not create a release tag.

## PostgreSQL validation ownership

| Retained work | Required evidence |
| --- | --- |
| Checkout, pinned Rust/nextest, system prerequisites, component cache | Validate this commit reproducibly with available generated-protobuf and database tooling; reuse dependencies |
| Dependency graph guards | PostgreSQL consumers compile without embedded SurrealDB; the opt-out graph has no OAuth, SSE, or GraphQL features |
| PostgreSQL 16 service and health check | Real migrations, transactions, row locking, uniqueness, permissions, and durable create intents |
| Four disposable HTTP namespaces | Allow the conditional, identity, event, and create-intent targets to run together without metadata or table collisions |
| Ordinary PostgreSQL package tests | Backend SQL/planning units and runtime/CLI behavior compiled with the PostgreSQL release features |
| Ignored live PostgreSQL tests | Storage concurrency/migration contracts plus six runtime HTTP, identity, event, reconciliation, and tenant-membership cases |
| Clippy with warnings denied | Lint all PostgreSQL consumer targets with the same features used by their tests |
| Parallel extension-disabled tests and lints | Prove that PostgreSQL alone leaves optional extension routes and export capabilities disabled |

Portable core, DSL, and backend unit tests/lints belong to the portable component
jobs. SurrealDB integration, including GraphQL units and real GraphQL fixtures,
belongs to the SurrealDB job. They are no longer separate packages in the
PostgreSQL job. Runtime and CLI tests remain in each backend job because their
compiled feature graphs differ, including backend-specific preparation and
security behavior. Extension-disabled checks retain a separate graph because
feature unification would otherwise invalidate the negative assertions. The
opt-out test build selects only the runtime library and CLI OpenAPI integration
target that contain those three checks; clippy still checks every target.

The live test invocation selects ignored tests only in `schema-forge-postgres`
and the runtime's `postgres_*` binaries. It intentionally leaves the existing
MinIO tests to their separate opt-in environment. Normal tests and live tests
use the same packages and features, so the second invocation reuses the compiled
binaries. Conditional and create-intent test pairs serialize within their own
namespaces while retaining the concurrent requests inside each test.

The old PostgreSQL workflow omitted the two PostgreSQL create-intent runtime
tests. They now run alongside the four previously executed runtime cases.
All six cases retain their assertions. Ephemeral service teardown already
removes the PostgreSQL database, so an explicit end-of-job schema cleanup and
routine disk report are unnecessary after removing the foreign engine builds.

To run the same graph locally, provision a disposable PostgreSQL instance with
`conditional_http`, `oauth_http`, `events_http`, and `create_intents_http` schemas.
Set `SCHEMAFORGE_TEST_POSTGRES_URL` for storage tests and the respective scoped
`SCHEMAFORGE_TEST_POSTGRES_HTTP_URL`, `SCHEMAFORGE_TEST_POSTGRES_IDENTITY_URL`,
`SCHEMAFORGE_TEST_POSTGRES_EVENTS_URL`, and
`SCHEMAFORGE_TEST_POSTGRES_CREATE_INTENTS_URL` values for the HTTP targets. Set
`SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1`. Each scoped URL uses an appropriate
`options=-csearch_path%3D<namespace>` query. The targets accept the old global URL
as a fallback when run individually; simultaneous targets require distinct
namespaces.

```sh
features=schema-forge-cli/postgres,schema-forge-cli/oauth,schema-forge-cli/sse
cargo nextest run --locked -p schema-forge-postgres -p schema-forge-acton -p schema-forge-cli --no-default-features --features "$features"
cargo nextest run --locked -p schema-forge-postgres -p schema-forge-acton -p schema-forge-cli --no-default-features --features "$features" --run-ignored only -E 'package(schema-forge-postgres) | (package(schema-forge-acton) & binary(/^postgres_/))'
cargo clippy --locked -p schema-forge-postgres -p schema-forge-acton -p schema-forge-cli --no-default-features --features "$features" --all-targets -- -D warnings
```

The other suites retain distinct evidence: portable compilation and doctests,
real SurrealDB and GraphQL behavior, SQL Server 2019/2022 contracts and native
Windows compilation/signature-tool compatibility, CEL proofs, generated React
build/lint/browser behavior, and release-platform packaging/signing. Release
validation checks the exact tag before distributing binaries. Metadata policy
and final result gates remain required even when component jobs are skipped.
