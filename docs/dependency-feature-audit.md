# Dependency feature audit

The October 6, 2026 audit removes unused dependency features while retaining
SchemaForge's supported backends, credential sources, cryptography, exports,
GraphQL routes, generated sites, and diagnostics. The lockfile drops from
1,059 to 1,038 external packages across all platforms, a net reduction of 21.
The workspace graph reported by `cargo metadata` drops from 1,016 to 997.
These are package counts, not compilation-unit counts or measured build-time
savings.

The baseline is commit `ee48b0a`. Registry manifests and `cargo info --verbose`
were checked alongside source, build scripts, generated protobuf consumers,
and target-specific feature graphs. `cargo machete` supplied candidates;
its generated-code and FIPS false positives were checked manually. All
dependency declarations were changed through `cargo add` or `cargo remove`.

| Dependency | Change | Usage that remains |
| --- | --- | --- |
| Runtime `zip` | Disable defaults; enable only `deflate-flate2-zlib-rs` | `export_bundle.rs` writes Deflate archives at the default compression level. ZIP encryption, Bzip2, Deflate64, LZMA, PPMd, XZ, Zstd, and timestamp integration are unused. XLSX dependencies retain their own ZIP features. |
| Runtime `async-graphql` | Disable defaults; retain `dynamic-schema` and `graphiql` | Dynamic schema construction and the GraphiQL GET route remain. The separate Playground UI, email validator, and multipart-upload tempfile feature have no callers. |
| Runtime `reqwest` | Disable defaults; retain `http2` and `rustls` | Webhook clients explicitly call `no_proxy()`, send already serialized bodies, and inspect status codes. Charset decoding, JSON helpers, and system-proxy discovery are unused here. CLI JSON and streaming features remain enabled by its own declaration. |
| CLI and test `tokio` | Replace `full` with explicit runtime, file, I/O, network, synchronization, and timer features at their call sites | Async CLI downloads, stdout streaming, actor tests, HTTP fixtures, and database tests retain their required APIs. No SchemaForge caller requires Tokio process spawning or parking-lot integration. |
| MSSQL `acton-service` | Disable framework defaults; retain `mssql` and `crypto-aws-lc-rs` | The backend uses `DatabaseConfig`, `create_pool`, and `MssqlPool`. The standalone backend does not run an HTTP or telemetry service. Server builds enable those capabilities through the CLI/runtime declarations. |
| PostgreSQL `sqlx` | Disable defaults; select `runtime-tokio`, `postgres`, `json`, `chrono`, `uuid`, and `tls-rustls-aws-lc-rs` | The adapter uses runtime queries and PostgreSQL row/argument types. It does not use `query!`, derived SQLx models, `Any`, or SQLx migration helpers. TLS explicitly uses the existing AWS-LC provider. |
| MSSQL test `testcontainers` | Remove `blocking` | SQL Server fixtures use `AsyncRunner`; no synchronous runner is used. |

SQLx remains on the latest 0.8 release, 0.8.6, matching acton-service's API line
and avoiding duplicate database driver stacks. Re-adding narrowed Tokio and
reqwest declarations selected current compatible patch releases 1.53.2 and
0.13.5. Cargo also selected rand 0.10.3 transitively. Reqwest 0.13.5 adds
base64 0.23.1, so the portable CLI's package count increases by one despite
its narrower direct Tokio declaration.

Eleven redundant declarations were removed:

| Workspace crate | Removed declarations | Why they are redundant |
| --- | --- | --- |
| `schema-forge-acton` | Build dependencies `prost-build`, `tonic-build` | Its build script calls `tonic-prost-build`, which owns those dependencies. |
| `schema-forge-cel` | `base64`, `tracing` | CEL's encoding functions call the core crate's encoding helpers; there are no direct tracing calls. |
| `schema-forge-cli` | Dev dependency `tower` | No CLI test imports or calls Tower directly. |
| `schema-forge-postgres` | `argon2`, `password-hash`, `rand`, `serde` | Password operations delegate to the backend crate; JSON conversions use `serde_json` and shared model types. |
| `schema-forge-signing` | `serde_json` | The signing crate has no direct JSON caller. |
| `schema-forge-surrealdb` | `serde` | Conversions use SurrealDB values, `serde_json`, and shared model types. |

Production and build dependency graphs on `x86_64-unknown-linux-gnu` show:

| Graph | Baseline packages | Audited packages | Reduction |
| --- | ---: | ---: | ---: |
| Portable CLI | 337 | 338 | -1 |
| Server CLI, OAuth, SSE | 606 | 593 | 13 |
| PostgreSQL CLI, OAuth, SSE | 628 | 614 | 14 |
| SurrealDB CLI, OAuth, SSE, GraphQL | 810 | 796 | 14 |
| MSSQL CLI, OAuth, SSE | 649 | 637 | 12 |
| Standalone PostgreSQL adapter | 304 | 297 | 7 |
| Standalone MSSQL adapter | 329 | 317 | 12 |
| Runtime with GraphQL | 585 | 571 | 14 |

Feature unification matters: [Cargo combines dependency feature requests](https://doc.rust-lang.org/cargo/reference/features.html#feature-unification).
`sigstore-trust-root` 0.7.0 still requests Tokio `full`, even in the portable
CLI. Removing SchemaForge's `full` declarations therefore clarifies its own
requirements without shrinking the currently unified Tokio build.
Acton-service 0.46.0's `database` feature still requests SQLx `any`, `macros`,
and `migrate`. The standalone PostgreSQL adapter loses those features, but
the complete server retains them. Further reductions require upstream
manifest changes and validation of upstream callers.

The PostgreSQL graph check now requires AWS-LC features on both SQLx and its
core driver. It rejects competing SQLx TLS provider features, including
features unified by another dependency, because SQLx 0.8.6 prefers Ring when
both providers are enabled. Other dependencies may still use Ring independently.

The audit retains AWS SSO and credential-process support because
`S3Client::from_backend_config` explicitly promises the default credential
chain when static credentials are absent. It retains the SDK's default
signing behavior, which can be selected by AWS endpoint/bucket configuration.
It also retains SurrealDB's embedded engine and remote transports, mTLS and
Windows authentication, observability, rate limiting, resilience, audit
logging, and framework configuration APIs. These are supported runtime
capabilities, not unused features inferred from missing direct imports.
Prost, tonic-prost, CEL prost-types, and FIPS aws-lc-rs remain despite static
analyzer warnings: generated protobuf code or crypto feature unification
requires them. MiniJinja macros are used by generated form templates; its
diagnostic defaults and miette's graphical error/backtrace support remain.
Core, config, codegen, DSL, and backend dependencies were reviewed without
finding another removal that preserves their supported behavior.

To repeat a graph inspection, select one supported backend at a time:

```sh
cargo info sqlx@0.8.6 --verbose
cargo tree --locked --target x86_64-unknown-linux-gnu \
  -e normal,build --prefix none --format '{p}|{f}' \
  -p schema-forge-cli --no-default-features --features postgres,oauth,sse
cargo tree --locked -e features -i sqlx \
  -p schema-forge-postgres
cargo tree --locked -e features -i tokio \
  -p schema-forge-cli --no-default-features
```

Production counts exclude dev dependencies. CI additionally validates
the narrowed test features, live PostgreSQL and SQL Server storage, SurrealDB
contracts, GraphQL, generated sites, native Windows compilation, and CEL proofs.
Compiler or lockfile changes legitimately create new dependency cache keys;
the shared cache maintenance workflow repopulates them after merge.
