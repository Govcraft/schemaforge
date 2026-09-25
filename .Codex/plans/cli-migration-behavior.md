# CLI migration and operator behavior

Apply Rust planner/author standards using existing domain and error types.

1. Plan every desired schema before apply/migrate execution and reject any destructive noninteractive batch before migration, metadata, or revision preparation writes. Preserve interactive per-schema consent and dry-run plans. Test mixed safe/destructive batches with zero writes.
2. Preserve configured listener host/port with optional flags. Keep SchemaForge loopback as its unconfigured host, respecting the framework config search and ACTON environment layers. Test omitted flags, explicit default overrides, and config-file bind/port.
3. Keep the existing public RequiresConfirmation enum variant for source compatibility, but label it review in Display/serialized output, accept legacy serialized spelling, and document it as informational consistently. Introduce plan-aware step classification for fresh unique constraints and test both existing/new schema uniqueness.
4. Add bounded 429 retries to the entity HTTP client, with --max-retries and Retry-After seconds/date support. Rebuild identical requests only for explicit 429 responses, no transport or 5xx retries. Test exhaustion, eventual success, non-429 refusal, and delay parsing.
5. Correct the rule-ordering reference and document governor defaults, reverse-proxy trust and probe configuration.
6. Coordinate read-only CLI connections and webhook validation with owning agents.

Validation: cargo nextest run for core and CLI with postgres feature, cargo clippy warnings denied, formatting. Root performs workspace integration and release. Semver recommendation: minor because migration machine-readable review labels change and CLI functionality is added; retain deserialization compatibility.
