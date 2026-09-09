# CEL verification scope

The CEL engine combines executable conformance examples with bit-precise scalar
proofs. Neither is a proof of the whole service or of every expression a user can
submit.

## Run locally

Install [Kani](https://model-checking.github.io/kani/install-guide.html) with rustup
available, and install `protoc` for the crate's conformance build script. Put
[Z3 5.1.0](https://github.com/Z3Prover/z3/releases/tag/z3-5.1.0) on `PATH` too.
The workflow pins and checksums the Linux x86_64 archive. Integer and temporal
harnesses select Z3; float harnesses retain Kani's bundled CaDiCaL because CBMC's
SMT float translation does not support all rounding operations. Do not globally
override the solver on the command line:

```sh
cargo install --locked kani-verifier --version 0.67.0
cargo kani setup
cargo kani -p schema-forge-cel -Z function-contracts --jobs 4 --output-format terse
cargo nextest run -p schema-forge-cel
cargo clippy -p schema-forge-cel --all-targets -- -D warnings
```

The independent `CEL scalar proofs` GitHub workflow runs the same verifier. It
is deliberately separate from the nextest test gate. Kani is a development tool;
there is no Kani Cargo dependency, feature in the shipped binary, or production
runtime requirement. Contract attributes and harnesses are guarded by `cfg(kani)`.
The manifest registers that cfg name without suppressing warnings.

## What the scalar proofs establish

`eval/scalar.rs` contains the actual functions called by `ops.rs` and `convert.rs`.
`eval/scalar_proofs.rs` verifies those functions directly. No production function
is replaced by a trusted stub. The numeric functions carry
[function contracts](https://model-checking.github.io/kani/reference/experimental/contracts.html),
checked by `proof_for_contract` harnesses. There are no numeric preconditions:
invalid inputs must return a range failure, so a `requires` restriction would
unnecessarily hide the very inputs these totality proofs need to cover.

The integer operands range over every `i64` or `u64` value, including the full
cross product for binary operations. Floating operands come from arbitrary `u64`
bit patterns, including NaNs, infinities, signed zero, subnormals and rounded
integer limits. Successful verification includes Kani's panic, overflow and
memory-safety checks as well as each assertion/postcondition.

- Signed and unsigned addition, subtraction and multiplication return the exact
  mathematical integer when representable and `None` otherwise. The mathematical
  model uses `i128`/`u128` so it cannot overflow on these inputs. The signed
  multiplication specification uses `i128::wrapping_mul`: two widened i64 values
  have a product in `[-2^126 + 2^63, 2^126]`, so wrapping is mathematically
  impossible. This avoids asking the solver to re-prove an unnecessary i128
  overflow instrumentation check. Production still calls `i64::checked_mul`.
- Division/remainder reject zero, and signed division/remainder reject `MIN` with
  `-1`. Otherwise the returned value equals Rust's defined integer operation.
  Negation rejects exactly `i64::MIN`.
- Mixed signed/unsigned comparison agrees with a widened signed integer model.
  Integer/double comparisons use an independent truncated `i128` model.
- Numeric conversion rejects unrepresentable values instead of silently
  saturating. `double` to `int` retains the conformance corpus's strict exclusion
  of both `-2^63` and `2^63`. `uint` truncates toward zero, so values between `-1`
  and zero convert to zero. Float-to-integer proofs check the exact round trip to
  the truncated float. Integer-to-double proofs check finiteness, sign and the
  maximum IEEE-754 rounding error (512 for i64, 1024 for u64).
- Duration nanosecond grouping is exact for all `i64` seconds and `i32`
  subsecond values using `i128`. Formatting consequently handles Chrono's full
  duration range without overflowing an `i64` intermediate, and regression
  tests preserve the minus sign on negative subsecond durations.

The temporal contracts cover full `i64` whole-second and `i32` fractional
nanosecond duration components, full `i64` timestamp seconds, all `u32`
timestamp nanoseconds and both addition/subtraction. Invalid timestamp
nanoseconds (`>= 1_000_000_000`) reject. Timestamp arithmetic also rejects
noncanonical duration fractions outside `-999_999_999..=999_999_999`. Valid
results must normalize the fraction, preserve the exact seconds plus fractional
carry/borrow through widened arithmetic, and fit either the CEL
`0001..9999` timestamp range or the Go i64 nanosecond duration range. These are
production kernels used by the Chrono adapters, not parallel mathematical models
substituted for the runtime implementation.

The Chrono adapter proofs have explicit finite bounds: timestamp inputs are the
two CEL endpoints and the two Chrono UTC endpoints; duration inputs are zero,
both Go i64 nanosecond endpoints and both Chrono duration endpoints. Every offset
from -128 through 127 nanoseconds is verified for addition and subtraction.
This proves the requested edge crossings and return-value/error behavior without
claiming a symbolic proof of all Chrono calendar code. Ordinary tests also cover
large cancellation into the CEL range and preserve the existing leap-second
adapter behavior. Leap-second encoding is outside the canonical protobuf scalar
contract. There are no recursive/loop harnesses and no unwinding checks disabled.

The evaluator translates numeric `None` results to its existing canonical CEL
errors. Unit tests and the conformance oracle check that public error mapping;
proofs concentrate on the heap-free scalar boundary.

## What is outside these proofs

The vendored cel-spec corpus remains the executable oracle for parsing, overload
selection, values and error strings. It checks enumerated examples, not all
possible inputs. These scalar harnesses do not symbolically parse arbitrary
strings, walk arbitrary recursive ASTs, verify allocations, or prove the entire
Chrono library. Parser/evaluator differential fuzzing remains follow-on work;
this change does not claim to have run or supplied that separate fuzz target.
