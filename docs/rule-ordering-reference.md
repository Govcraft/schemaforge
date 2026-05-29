# Rule ordering and signed-rule audit reference

SchemaForge enforces declarative, write-time rules — `@default`, `@compute`, and
`@require` — directly from a `.schema` file (see the [Hooks
Reference](hooks-reference.md) for the separate, out-of-process gRPC hook
mechanism). Two readers should jump to the relevant section by heading:
application authors who need to know **exactly when** each rule runs relative to
hooks and persistence, and security auditors who need to know **how** those
rules are bound to a signed artifact and how to enumerate every rule gating an
entity. Scope is the runtime ordering contract and the audit/provenance story;
the [Signing Reference](signing-reference.md) covers the signing CLI, trust
bundles, and rollout.

## 1. Canonical write-path ordering (issue #105)

For every entity create, update (PUT), and patch (PATCH), the engine executes a
single, fixed, engine-controlled sequence. The order does not depend on schema
authoring or on field declaration order across phases:

```text
@default  →  @compute  →  @require  →  before_* hooks  →  PERSIST  →  { after_* hooks, webhook dispatch }
└───────────── rule phases (in-transaction) ─────────┘   └ network ┘   └────────── detached fan-out ──────────┘
```

The three rule phases live in `crates/schema-forge-acton/src/rules.rs`
(`apply_defaults`, `apply_computed`, `check_requires`); the route handlers in
`crates/schema-forge-acton/src/routes/entities.rs` call them in this order
before dispatching any `before_*` hook.

### 1.1 Why rules run before hooks

The rule phases are **pure and cheap**: they evaluate CEL against the in-memory
field set with no I/O. Running them first means a `@require` rejection
short-circuits the entire write **before** any `before_*` gRPC hook network
round-trip and **before** anything is persisted. A request that violates a rule
never costs a hook call.

### 1.2 The invariants

| Invariant | Guarantee |
|---|---|
| In-transaction, pre-persistence | All three rule phases run before the backend write, inside the same request that persists. |
| Rules ahead of `before_*` hooks | `@default`/`@compute`/`@require` all complete before the first `before_*` hook is dispatched. |
| Deterministic, no reentrancy | Phases run in the fixed order above; each visits fields in schema declaration order; a phase never re-invokes an earlier phase. |
| Rejection suppresses all downstream work | A `@require` failure returns **422** and fires **no** `before_*` hook, persists **nothing**, and therefore fires **no** `after_*` hook and **no** webhook. |
| Fan-out is detached | `after_*` hooks (via the `HookDispatchActor`) and webhook delivery (spawned by the webhook dispatcher) never block the API response. |

### 1.3 Phase notes

- **`@default`** is *insert-only* — it runs on create only, never on PUT/PATCH,
  and only fills a field that is absent or explicitly `null`. On create it runs
  *after* the engine stamps owner/tenant/audit columns, so those injected
  non-null values win over an expression `@default` for the same field.
- **`@compute`** is server-derived and **overwrites** any client-supplied value
  for the computed field. It rebuilds its CEL bindings from the current field
  map before each field, so a later compute can read an earlier one (chaining).
  Because it runs after `@default`, a compute can read a defaulted sibling.
- **`@require`** runs last, so its predicates validate the *finalized* field set
  — including computed values. It is **fail-closed**: a predicate passes only on
  `Ok(true)`; a definite `false` is a 422 rejection, and an error or non-boolean
  result is a 500 (a broken predicate can never let a write through).

This ordering is proven by integration tests:
`rule_phase_order_default_then_compute_then_require_is_observable` (in
`crates/schema-forge-acton/tests/integration.rs`) and
`require_rejection_fires_no_before_or_after_hook_and_persists_nothing` /
`passing_require_reaches_before_and_after_hooks` (in
`crates/schema-forge-acton/tests/hooks_integration.rs`).

## 2. Rules as part of the signed artifact (issue #106)

A SchemaForge rule is **declarative annotation text inside the `.schema`
file** — for example:

```text
schema Invoice {
    @default("0")
    subtotal: float
    @compute("subtotal * 1.1")
    total: float
    @require("total >= 0", "total must be non-negative")
    status: text
}
```

This is the audit win over an out-of-band gRPC hook: a hook's logic lives in a
separately-deployed binary that the schema signature does not cover, whereas a
rule's logic is bytes in the signed `.schema` file.

### 2.1 How the signature covers the rules

Signed-schema enforcement (see the [Signing Reference](signing-reference.md))
binds the **raw bytes** of each `.schema` file to a signer:

1. `schema-forge-signing` computes `sha256(file_bytes)` over the entire `.schema`
   file and pins it in `schemas.manifest.toml`.
2. A per-file `<file>.schema.sig` signs those same raw bytes.
3. On load under `mode = "warn"` / `"enforce"`, the verifier
   (`VerifyPolicy::verify_files`) re-reads each file, recomputes the hash, checks
   it against the manifest, and verifies the signature against the trust policy.

Because the `@default` / `@compute` / `@require` annotation text is part of those
raw bytes, **any change to a rule expression changes the file hash and the
signature no longer matches**. An attacker who weakens a `@require` threshold
(say, `total >= 0` → `total >= -999999`) cannot do so without invalidating the
signature, and under `enforce` the load aborts (exit code 13). This is covered by
`enforce_mode_rejects_tampered_rule_annotation` in
`crates/schema-forge-signing/src/policy.rs`.

### 2.2 How a reviewer enumerates every rule gating an entity

The rules that gate an entity are exactly the `@default` / `@compute` /
`@require` annotations on that entity's fields in its signed `.schema` file. To
enumerate them with provenance:

1. **Read the rules.** Open the signed `.schema` file (in the PR diff or on
   disk). Every gating rule is a `@require` (rejects writes), `@compute`
   (server-derived value), or `@default` (insert-time seed) annotation. There are
   no hidden rules — there is no rule source other than the `.schema` text.
2. **Surface them mechanically.** `sf parse <dir>` parses the `.schema` files and
   reports the typed schema, including the annotations, so a reviewer does not
   have to eyeball raw text. (`sf parse` also runs the verifier under the
   configured mode, so a tampered file is reported there too.)
3. **Confirm provenance.** `sf verify` (or any load under `enforce`) checks the
   per-file signature and manifest hash against the trust bundle. A passing
   verification means the rules the reviewer just read are the exact rules that
   will run — signed by a trusted identity, unmodified since.

The pairing is the point: step 1–2 tell the reviewer *what* the rules are, and
step 3 gives them cryptographic assurance that *those* rules — not a tampered
variant — are what the running server enforces.
