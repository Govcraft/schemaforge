# 0001 — `uint`: distinct DSL type vs. unsigned constraint

## Status

Accepted

## Context

CEL treats `uint` (unsigned 64-bit) as a **distinct type**: it has its own
overflow rules and there is no implicit `int`↔`uint` conversion. SchemaForge's
DSL, by contrast, collapses all integers to a single `FieldType::Integer(i64)`
(`crates/schema-forge-core/src/types/field_type.rs`). Issue #98 asks whether
SchemaForge should model unsigned as a **distinct `FieldType`** or as an
**`IntegerConstraints { min: 0 }` constraint** on the existing `Integer` type.

The tension stated in #98 is that "CEL conformance treats the type distinction
as load-bearing, so a constraint cannot fake it." We checked that claim against
the code and found it does **not** apply to the DSL surface, for one reason:

- **The conformance distinction lives in the engine, not in the DSL.** The CEL
  evaluator already carries a separate `CelValue::Uint(u64)` / `CelType::Uint`,
  distinct from `Int`, with a deliberately type-exact derived `PartialEq` so the
  conformance oracle rejects `Int(1)` where `Uint(1)` is expected
  (`crates/schema-forge-cel/src/value/mod.rs`, see the module-doc note and the
  `cel_type` mapping). CEL `uint` conformance (`integer_math`, comparisons, etc.)
  is therefore satisfied at the value/evaluator layer **regardless** of how the
  DSL spells a stored field. Choosing a DSL representation for unsigned fields
  does not change the conformance pass rate.

Other load-bearing facts from the code:

- **`min: 0` is already expressible with zero new code.**
  `IntegerConstraints` already carries `min: Option<i64>` and `max: Option<i64>`,
  with an `IntegerConstraints::with_min(0)` constructor
  (`crates/schema-forge-core/src/types/integer_constraints.rs`). A "non-negative
  integer" is `integer(min: 0)` today.

- **Neither storage backend has a native u64.** Postgres maps
  `FieldType::Integer` to `BIGINT`, a *signed* `i64`
  (`crates/schema-forge-postgres/src/codegen.rs::field_type_to_pg`), and emits a
  `CHECK ("field" >= 0)` automatically when `min` is set
  (`field_check_constraints`). SurrealDB maps it to `int`, also `i64`-based
  (`crates/schema-forge-surrealdb/src/codegen.rs`, `value.rs`
  `Number::Int(i64)`), and emits the equivalent `ASSERT $value >= 0`. Postgres
  has no native unsigned 64-bit integer; representing the full u64 range above
  `i64::MAX` would require `NUMERIC` (with the read/write/indexing complications
  that brings) on Postgres and a comparable workaround on SurrealDB.

- **The full u64 range cannot round-trip into storage even today.** The bridge
  surfaces a stored integer to predicates as `CelValue::Int`
  (`dynamic_to_cel`), and writes a CEL `uint` back via
  `i64::try_from`, returning `ConversionError::Overflow` for any value above
  `i64::MAX` (`crates/schema-forge-cel/src/value/bridge.rs::cel_to_dynamic`).
  A distinct DSL `Uint` field type would hit this same `i64`-shaped storage
  ceiling on both backends; it would not, by itself, unlock the `> i64::MAX`
  range.

- **The real-world need is "non-negative," not "full u64."** Counts, ages,
  quantities, and similar fields are non-negative integers well within `i64`,
  which `min: 0` covers exactly. A genuine requirement for the `> i64::MAX`
  range has not been demonstrated.

- **`FieldType` is `#[non_exhaustive]`.** A distinct `Uint` variant can be added
  later without a breaking change if a concrete u64-range requirement appears, so
  deferring the distinct type is reversible.

## Decision

Model unsigned integers as an **`IntegerConstraints { min: 0 }` constraint on
the existing `FieldType::Integer`**. Do **not** introduce a distinct DSL
`uint`/`Uint` field type at this time.

## Consequences

- A "non-negative integer" field is written `integer(min: 0)` and is enforced at
  the storage layer by an automatically generated `CHECK`/`ASSERT` on both
  Postgres and SurrealDB. No new DSL surface, no new column/DB type, no new
  bridge code.
- Values above `i64::MAX` are **out of scope** until a concrete requirement is
  shown. This matches the storage backends (both `i64`-bound) and the bridge,
  which already cannot round-trip `> i64::MAX` into storage.
- Rule expressions over these fields see CEL `int`, **not** `uint`. Authors who
  need `uint` literal/operator semantics inside an expression can use a `uint(x)`
  conversion in the expression; the engine's `Uint` machinery remains available
  at the value layer and is unaffected by this decision.
- CEL conformance is **unchanged** by this decision — the `Int`/`Uint`
  distinction the oracle grades on lives in the evaluator's value model, not in
  the DSL field type.
- The decision is **reversible**: because `FieldType` is `#[non_exhaustive]`, a
  distinct `Uint` variant can be added non-breakingly if and when a u64-range
  need is demonstrated, at which point the storage backends would also need an
  explicit `NUMERIC`/workaround mapping.

## Alternatives considered

- **Distinct `FieldType::Uint(...)` variant.** Rejected for now. It adds DSL
  surface, requires new mappings in both storage backends (neither has a native
  u64, so Postgres would need `NUMERIC` or a `CHECK`-bounded `BIGINT`) and new
  bridge handling, yet delivers no conformance benefit (the engine already
  distinguishes `uint`) and does not by itself unlock the `> i64::MAX` range
  given the `i64`-shaped storage and bridge. It can be added later without a
  breaking change, so committing to it now would be premature.

- **No constraint at all (status quo `Integer`).** Rejected. It fails to express
  or enforce the common "non-negative" requirement (counts/ages/quantities),
  which `min: 0` already supports with zero new code.

## References

- Issue #98 — uint: distinct type vs. unsigned constraint (this decision)
- Issue #89 — Epic: built-in declarative rules engine (escalation ladder;
  rules as a pure, signed, auditable control)
- Issue #91 — substrate decision: own CEL evaluator over `DynamicValue`
  (records context for the type decisions, including #98)
- Issue #90 — CEL conformance oracle (the cel-spec subset the engine is graded
  against)
- `crates/schema-forge-core/src/types/integer_constraints.rs` — `min`/`max`,
  `with_min`
- `crates/schema-forge-core/src/types/field_type.rs` — `FieldType`
  (`#[non_exhaustive]`), `Integer(IntegerConstraints)`
- `crates/schema-forge-cel/src/value/mod.rs` — distinct `CelValue::Uint` /
  `CelType::Uint`; type-exact `PartialEq` for the oracle
- `crates/schema-forge-cel/src/value/bridge.rs` — `dynamic_to_cel` (stored int →
  `Int`), `cel_to_dynamic` (`Uint` → `Integer` with `i64` overflow check)
- `crates/schema-forge-postgres/src/codegen.rs` — `Integer` → `BIGINT`;
  `min`/`max` → `CHECK`
- `crates/schema-forge-surrealdb/src/codegen.rs` — `Integer` → `int`;
  `min`/`max` → `ASSERT`
