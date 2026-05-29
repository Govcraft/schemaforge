# Architecture Decision Records

This directory holds SchemaForge's Architecture Decision Records (ADRs): short, durable notes that capture a non-trivial design decision, the context that forced it, and the consequences we accept by making it. ADRs are part of the auditable record (ATO trail), so each one should be self-contained, grounded in the actual code, and honest about trade-offs. Files are numbered and named `NNNN-title.md`; each carries a **Status** of `Proposed`, `Accepted`, or `Superseded` (a superseded ADR names its replacement). To add one, take the next number, follow the standard structure (Title, Status, Context, Decision, Consequences, Alternatives considered, References), and link the driving issue(s).

## Index

- [ADR-0001](0001-uint-type-vs-unsigned-constraint.md) — `uint`: distinct DSL type vs. unsigned constraint (issue #98)
- [ADR-0002](0002-cel-expression-substrate.md) — CEL expression substrate: own evaluator over `DynamicValue` (issue #91)
- [ADR-0003](0003-export-capability.md) — Export capability: bulk export as an authorized, fail-closed query (epic to be filed)
