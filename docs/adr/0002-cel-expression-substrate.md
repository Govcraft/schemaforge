# 0002 — CEL expression substrate: own evaluator over `DynamicValue`

## Status

Accepted

## Context

SchemaForge needs a CEL (Common Expression Language) evaluation layer to power
its declarative rules engine (issue #89). Three options were on the table:

1. **Depend on the upstream `cel` crate** — the most obvious path for any Rust
   project that needs CEL evaluation.
2. **Vendor the `cel` crate** — take a snapshot, own the diff, ship it inside
   the repo.
3. **Build a minimal, first-party evaluator over `DynamicValue`** — write only
   what SchemaForge needs, fully owned, verified against the published CEL
   conformance spec.

The selection is not academic. SchemaForge is a US-government production target
on an ATO (Authority to Operate) track. That context forces constraints that
rule out options 1 and 2 before any feature comparison:

- **Supply-chain posture.** A federal product must minimize its dependency tree
  and be able to fully vet every dependency it ships. The `cel` crate's
  canonical Rust implementation was last actively maintained through 2023; the
  library carries no conformance gate and has no active upstream steward. An
  unmaintained transitive dependency with no conformance coverage is an audit
  liability that reviewers will flag.

- **FIPS / airgap requirements.** The engine must build and run in airgapped,
  FIPS-constrained environments. Any dependency that could introduce a surprise
  network pull, non-FIPS primitive, or unvetted native extension is
  disqualifying.

- **Scope fit.** SchemaForge needs a CEL substrate that integrates directly with
  its own `DynamicValue` type. The `cel` crate's design would be inherited
  wholesale — including its gaps and its conceptual surface area — rather than
  arriving at a shape that exactly fits the project.

- **Cedar is already present for authorization.** Cedar is scoped to policy
  evaluation and produces boolean authorization decisions. It does not evaluate
  expressions to typed values and must not be overloaded for that purpose.

## Decision

Build a **first-party, minimal CEL evaluator over `DynamicValue`**, fully owned
by the SchemaForge project. The engine is implemented across three focused
issues:

- **#107** — CEL lexer + parser → typed AST (`schema-forge-cel` crate)
- **#108** — tree-walking evaluator core
- **#109** — standard function library (stdlib)

Conformance is verified **test-first** against the `cel-spec` simple corpus,
vendored as a **build/test-time oracle only** — the corpus is never a runtime
dependency and is never shipped with the binary. This oracle is introduced in
issue #90 and currently gates approximately 2 123 workspace tests.

The `cel` crate (upstream) is not depended upon, vendored, or referenced at
runtime. Cedar continues to serve only authorization; its scope does not expand.

## Consequences

- The `schema-forge-cel` crate is wholly owned by this project. There is no
  upstream to track, no vendored snapshot to audit on every update, and no
  transitive dependency that could surface as a supply-chain finding during ATO
  review.
- The engine builds and runs with no runtime network access and no dependency on
  non-FIPS primitives, satisfying airgap and FIPS environment requirements.
- Conformance is durable: the cel-spec corpus (issue #90) is a permanent
  regression gate. Any future engine change that breaks a conformance case will
  fail CI before it merges.
- Full ownership means the project carries maintenance. There is no upstream to
  pull bug-fixes or new CEL spec features from; those must be implemented
  in-house when needed. This is an accepted cost given the supply-chain and
  auditability benefits.
- The decision is **already in effect**: issues #107, #108, and #109 are
  complete. This ADR records the substrate context that informs the downstream
  type-projection decisions in issue #98 (uint, ADR-0001) and issue #102.
- Because the conformance corpus is a test-time oracle and not a shipped
  artifact, it imposes no runtime or distribution constraint.

## Alternatives considered

- **Live upstream dependency on the `cel` crate.** Rejected. The crate has been
  effectively unmaintained since 2023, carries no conformance gate, and an
  unvetted, unmaintained transitive dependency is an explicit audit liability for
  a federal product seeking ATO. Supply-chain posture alone is disqualifying,
  independent of any feature gaps.

- **Vendoring the `cel` crate.** Rejected. Vendoring a snapshot would require
  fully vetting the crate's existing design, gaps, and code at intake, and
  re-vetting on every update. It would also mean inheriting a design that does
  not naturally fit `DynamicValue`. Building exactly what SchemaForge needs from
  scratch is cleaner: the scope is bounded, the shape is right-fitted, and there
  is no inherited debt to audit. The effort to vet a vendored snapshot is not
  materially less than building a purpose-fit implementation.

- **Cedar for expression evaluation.** Hard no. Cedar is scoped to authorization
  policy and produces boolean decisions; it does not evaluate expressions to
  typed values. Overloading Cedar for expression evaluation would conflate two
  distinct concerns (authz vs. data-layer predicate evaluation), complicate the
  Cedar policy model, and produce an architecture that is difficult to explain
  and audit. Cedar's scope does not change.

## References

- Issue #89 — Epic: built-in declarative rules engine (escalation ladder)
- Issue #90 — CEL conformance oracle (cel-spec corpus as test oracle)
- Issue #91 — Substrate decision: own CEL evaluator over `DynamicValue` (this
  decision)
- Issue #98 — `uint`: distinct type vs. unsigned constraint (ADR-0001; depends
  on this substrate)
- Issue #102 — Type-projection decisions (depends on this substrate)
- Issue #107 — CEL lexer + parser → typed AST
- Issue #108 — Tree-walking evaluator core
- Issue #109 — Standard function library (stdlib)
- `crates/schema-forge-cel/` — first-party CEL engine crate
- `crates/schema-forge-cel/src/value/mod.rs` — `CelValue` / `CelType`;
  `DynamicValue` integration
- ADR-0001 — `uint`: distinct DSL type vs. unsigned constraint
