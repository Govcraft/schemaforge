# SurrealDB prefix query compatibility

The packaged authenticated smoke test reached storage and reproduced a parse error: the query builder emits `string::startsWith`, while the pinned SurrealDB 3.3.0 server requires `string::starts_with`.

Change only the `Filter::StartsWith` function spelling in `crates/schema-forge-surrealdb/src/query.rs` and its existing SQL expectation. Preserve field-path handling and string escaping.

Add a real-server regression to `crates/schema-forge-surrealdb/tests/data_correctness.rs`, using the existing isolated test-support connection and schema/entity APIs. Insert matching and nonmatching text values, query ordinary, apostrophe-containing, and backslash-containing prefixes, and assert exact matching rows and total counts. Also check pagination retains the full filtered count.

No new types, dependencies, errors, or public API are needed. This restores an existing filter operation and requires a patch release; root owns version updates. Validate with locked Nextest through the disposable SurrealDB test runner, package Clippy with warnings denied, and formatting checks. Root owns the packaged authenticated smoke and release validation.
