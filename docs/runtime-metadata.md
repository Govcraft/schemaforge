# Runtime and release metadata

`GET /api/v1/forge/meta` is public and returns deployment identity in `build`:

| Field | Meaning | Suggested UI label |
| --- | --- | --- |
| `version` | Cargo package version of `schema-forge-acton`; existing semantics are preserved. | Runtime version |
| `release_version` | Version supplied by the embedding binary. Official SchemaForge binaries use the CLI package version, matching `schemaforge --version`. | Release version |
| `source_revision` | Source revision supplied at binary build time. Official release CI embeds the full Git commit of its checked-out source. | Source revision |

Release and runtime versions can differ because the crates are versioned independently. Use `release_version` to locate the published SchemaForge release, and `source_revision` to locate its exact source commit. `/health` continues to report the runtime component version.

Both new fields are always present, with JSON `null` when unavailable. Administrative UIs should display an unavailable label or omit that detail when null, rather than treating the runtime version as the release version.

## Development and downstream binaries

`MetaInfo::new(...)` leaves both deployment fields unavailable. Downstream binaries can supply their own identity without depending on SchemaForge CLI types:

```rust
let meta = schema_forge_acton::MetaInfo::new("postgres", "PostgreSQL", 3600)
    .with_release_metadata(
        Some(env!("CARGO_PKG_VERSION")),
        option_env!("SCHEMAFORGE_SOURCE_REVISION"),
    );
```

Either argument can be `None`. Empty or whitespace-only strings are normalized to `None`; known values are trimmed. No release or revision is inferred from the runtime component version.

The SchemaForge CLI always supplies its package version. A development build without `SCHEMAFORGE_SOURCE_REVISION` reports `source_revision: null`. To supply a known revision for a build:

```sh
SCHEMAFORGE_SOURCE_REVISION="$(git rev-parse HEAD)" cargo build -p schema-forge-cli
```

This variable is read at compile time, not server startup. Setting it identifies the supplied revision; it does not certify a clean working tree. Official CI stamps the checked-out commit before building each release artifact.
