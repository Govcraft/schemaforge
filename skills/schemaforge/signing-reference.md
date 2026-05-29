# SchemaForge — Signed-Schema Enforcement Reference

Operator reference for `[schema_forge.signing]` and the `schemaforge sign` / `verify` / `trust-bundle` subcommands. Mirrors `docs/signing-reference.md` in the schemaforge repo; load that file (or this one) when wiring CI, configuring trust, or staging the off → warn → enforce rollout.

Shipped in `schema-forge-cli` 0.29.0 (Phases 1–5 of the signed-schema design). Every command that consumes `.schema` files (`apply`, `migrate`, `serve`, `parse`, `export`, `policies`, `hooks`, `site`) routes through the verifier.

## Threat model

Signed-schema enforcement defeats two attacks against any deployment where the schema directory is writable by an identity other than the signer.

1. **On-disk tampering** — anyone with filesystem write access to `schemas/` can modify entity definitions, Cedar policies, hook annotations, or the write-time rule expressions (`@require`/`@compute`/`@default`) between releases. Because the signature covers the full `.schema` bytes, the rule-annotation text is bound too: altering a `@require` predicate after signing fails verification under `enforce` (a hash mismatch), so an attacker cannot weaken a validation rule without re-signing. Hash-checking each `.schema` against a signed manifest binds the bytes SchemaForge loads to the bytes the signer approved.
2. **Untrusted-author provenance** — without signatures, no cryptographic binding exists between a `.schema` and an authorized identity. Per-file signatures verified against a trust policy give every schema an attributable author drawn from a fixed allowlist.

## Verifier checks (run in order)

Under `mode = "warn"` or `mode = "enforce"`:

1. Load `schemas.manifest.toml`. Verify `schemas.manifest.toml.sig` against the trust policy.
2. Cross-check directory contents against manifest entries — no extra `.schema` files on disk, no manifest entries missing from disk.
3. For each `.schema`:
   - Verify the per-file `<file>.sig` against the trust policy (OR-semantics across signers).
   - Confirm `sha256(file) == manifest[file].sha256`.
4. Under `enforce`, any failure aborts with exit code **13**. Under `warn`, failures log at ERROR and the process continues.

| Exit code | Meaning |
|---|---|
| `0` | Verification passed, or `mode = "off"`, or `mode = "warn"` regardless of result |
| `13` | Verification failed: bad signature, hash mismatch, missing manifest, extra/missing file on disk, or `--no-verify` refused under `enforce` |

## On-disk layout

Each `.schema` has a sibling `.sig`. The manifest and its signature sit alongside.

```text
schemas/
  user.schema
  user.schema.sig
  document.schema
  document.schema.sig
  schemas.manifest.toml
  schemas.manifest.toml.sig
```

For `cosign-keyless`, the `.sig` files contain a Sigstore Bundle JSON (`application/vnd.dev.sigstore.bundle.v0.3+json`) rather than raw signature bytes. Same filename, different payload format; the verifier auto-detects. Bundles embed the Rekor inclusion proof, which preserves the historical signing time the 10-minute Fulcio cert needs to validate long after expiry.

### Manifest format

`schemas.manifest.toml`:

```toml
schema_version = 1

[[entries]]
path = "user.schema"
sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

[[entries]]
path = "document.schema"
sha256 = "..."
```

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | integer | Must be `1` |
| `entries[].path` | string | Repository-relative path of the `.schema` |
| `entries[].sha256` | string | Lowercase hex SHA-256 of the file bytes |

## Trust policy — `[schema_forge.signing]`

Lives in `config.toml`, or in a standalone TOML file referenced by `--trust-policy`.

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `mode` | string | no | `"off"` | One of `off`, `warn`, `enforce` |
| `trust_root_bundle` | string (path) | no | embedded snapshot | Pre-fetched Sigstore TUF trust root JSON. Affects only `cosign-keyless` verifiers. |
| `trusted_signers` | array of tables | yes when `mode != "off"` | — | OR-semantics: a file passes if any entry's verifier accepts |

### Signer kind: `ed25519`

Raw ed25519 public key embedded in the policy. No external files, no network — suited to airgap.

| Field | Required | Meaning |
|---|---|---|
| `kind = "ed25519"` | yes | Tag |
| `name` | yes | Operator-chosen label; surfaces in audit logs |
| `public_key_b64` | yes | SPKI base64 (DER-encoded `SubjectPublicKeyInfo`) — the value `--print-pubkey` emits |

```toml
[[schema_forge.signing.trusted_signers]]
kind = "ed25519"
name = "roland-airgap-key"
public_key_b64 = "MCowBQYDK2VwAyEA..."
```

Rotation: generate a new keypair with `schemaforge sign --ed25519-generate --print-pubkey`, append the new block alongside the old one during overlap, re-sign every schema with the new key, then remove the old block.

### Signer kind: `ssh-allowed-signers`

Reuses an existing OpenSSH allowed-signers file — the same format `git config gpg.ssh.allowedSignersFile` consumes.

| Field | Required | Meaning |
|---|---|---|
| `kind = "ssh-allowed-signers"` | yes | Tag |
| `path` | yes | Filesystem path to an allowed_signers file |

SchemaForge signatures use namespace **`schema-forge-signing@govcraft.ai`**. The allowed-signers file supports `namespaces="..."`, `valid-after`, and `valid-before` per-line options; a key restricted to a different namespace or outside its validity window is rejected at the policy layer before the cryptographic check runs.

```text
roland@govcraft.ai namespaces="schema-forge-signing@govcraft.ai" ssh-ed25519 AAAAC3Nz...
```

```toml
[[schema_forge.signing.trusted_signers]]
kind = "ssh-allowed-signers"
path = "/etc/schemaforge/allowed_signers"
```

Rotation: add the new key line above the old one, set `valid-before` on the old line, re-sign during overlap, then delete the old line.

### Signer kind: `cosign-keyless`

Sigstore keyless verification. Requires a trust root (embedded snapshot by default, or `trust_root_bundle` for airgap).

| Field | Required | Meaning |
|---|---|---|
| `kind = "cosign-keyless"` | yes | Tag |
| `name` | yes | Operator-chosen label |
| `issuer` | yes | Exact-match OIDC issuer URL on the Fulcio cert |
| `subject_pattern` | yes | Glob matched against the OIDC subject (workflow URL, email, etc.) |

Verification rides the full Sigstore chain: cert against Fulcio root, SCT, Rekor inclusion proof, signature. The cert's OIDC subject is then matched against `subject_pattern` as a glob.

```toml
[[schema_forge.signing.trusted_signers]]
kind = "cosign-keyless"
name = "schemaforge-release-ci"
issuer = "https://token.actions.githubusercontent.com"
subject_pattern = "https://github.com/govcraft/schemaforge/.github/workflows/release.yml@refs/tags/v*"
```

Rotation: tighten or replace `subject_pattern` to point at the new workflow path or tag prefix; Fulcio short-lived certs need no manual rotation.

## Modes

| Mode | Verifier behavior | On failure | Use |
|---|---|---|---|
| `off` | Skipped; one-time WARN logged at startup | N/A | Bootstrap and pre-rollout |
| `warn` | Full check runs | Logs at ERROR, returns 0 | Migration observation window |
| `enforce` | Full check runs | Aborts, exit 13 | Production |

The shipped `schemaforge init` scaffold leaves the `[schema_forge.signing]` block fully commented out, so the default for fresh projects is effectively `off`. The commented stub uses `mode = "warn"` as the recommended starting point when the block is uncommented.

## CLI

### Global flags

| Flag | Effect |
|---|---|
| `--trust-policy <path>` | Override `[schema_forge.signing]` with a standalone TOML — one `config.toml` can fan out to many environments without duplicating database settings |
| `--no-verify` | Skip verification for one invocation. Refused under `mode = "enforce"` unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set |

| Env var | Effect |
|---|---|
| `SCHEMAFORGE_ALLOW_NO_VERIFY` | When `1`, permits `--no-verify` under `enforce`. Intended for one-off recovery, not steady-state CI |

### `schemaforge sign <PATHS>...`

Produces per-file `.sig` and a signed `schemas.manifest.toml` for each path.

| Flag | Meaning |
|---|---|
| `--ed25519-generate` | Generate a fresh ed25519 keypair, sign with it, print the private key path |
| `--ed25519-key <path>` | Sign with an existing ed25519 PKCS#8 private key |
| `--ssh-key <path>` | Sign with an existing OpenSSH private key (SSHSIG format, same as `ssh-keygen -Y sign`) |
| `--ssh-principal <id>` | Principal label printed in the allowed-signers advisory output |
| `--keyless` | Shell out to `cosign sign-blob --yes --bundle`; on-disk `.sig` becomes a Sigstore Bundle |
| `--cosign-bin <path>` | Override the `cosign` binary location (default `cosign`) |
| `--print-pubkey` | Print the `[[schema_forge.signing.trusted_signers]]` block matching the chosen scheme |

```bash
schemaforge sign schemas/ --ed25519-generate --print-pubkey
schemaforge sign schemas/ --ssh-key ~/.ssh/id_ed25519 --ssh-principal roland@govcraft.ai
schemaforge sign schemas/ --keyless    # CI: rides ambient OIDC token (GHA, Buildkite, etc.)
```

### `schemaforge verify <PATHS>...`

Standalone verifier. Touches no database. Exit 0 = clean; exit 13 = at least one failure. Suitable as a pre-merge CI gate.

```bash
schemaforge verify schemas/
```

### `schemaforge trust-bundle refresh`

Performs a full Sigstore TUF fetch on a connected host and serializes the resulting `TrustedRoot` to disk as pretty-printed JSON. Used for airgap / SCIF preparation.

| Flag | Meaning |
|---|---|
| `--output <path>` | Destination path for the trust root JSON (required) |
| `--instance <name>` | One of `public-good` (default), `staging`, `github` |
| `--force` | Overwrite an existing file at `--output` |

```bash
schemaforge trust-bundle refresh --output trust_root.json
```

### `schemaforge trust-bundle inspect <path>`

Prints a one-line `fulcio_certs=N rekor_keys=M tsa_certs=K` summary; confirms the bundle is non-empty before deployment.

```bash
schemaforge trust-bundle inspect /etc/schemaforge/trust_root.json
```

## Rollout playbook (off → warn → enforce)

Three stages, each gated on observable exit criteria. **Do not skip stages.**

### Stage 1 — Sign every schema (`mode = "off"`)

Precondition: clean working tree with all current `.schema` files committed.

- Pick one signer kind from the three above.
- Run `schemaforge sign schemas/ --print-pubkey` against the chosen scheme; capture the printed trust-anchor block.
- Commit the produced `.sig` files and `schemas.manifest.toml{,.sig}`.
- Paste the trust-anchor block into `config.toml`; keep `mode = "off"` (or leave the block commented out).

Exit criteria: every schema in the directory has a sibling `.sig`; `schemaforge verify schemas/` exits 0; CI builds still succeed.

### Stage 2 — `mode = "warn"`

Precondition: stage 1 exit criteria met across at least one full CI run.

```toml
[schema_forge.signing]
mode = "warn"

[[schema_forge.signing.trusted_signers]]
# ... pasted from stage 1
```

Run `schemaforge verify schemas/` locally and in CI. Verifier failures log at ERROR but do not abort. Exit criteria: zero verifier-failure log lines across a full release cycle.

### Stage 3 — `mode = "enforce"`

Precondition: stage 2 exit criteria met.

```toml
[schema_forge.signing]
mode = "enforce"
```

Failures abort with exit code 13. `--no-verify` is refused unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set. Exit criteria: a tampered-schema test in staging exits 13; clean schemas exit 0.

## Airgap and SCIF deployments

Only `cosign-keyless` needs an offline trust root. The `ed25519` and `ssh-allowed-signers` kinds carry their public material inside the trust policy, so they are inherently offline — no network access at any phase. **Deployments free to choose the signer kind should prefer one of these for SCIF environments.**

For `cosign-keyless` inside an airgap, fetch the trust root on a connected host and carry it across:

```bash
schemaforge trust-bundle refresh --output trust_root.json
schemaforge trust-bundle inspect trust_root.json
```

Transfer `trust_root.json` across the airgap by approved means (CD-R, one-way diode, etc.). On the airgapped host, place it at a stable path and point the policy at it:

```toml
[schema_forge.signing]
trust_root_bundle = "/etc/schemaforge/trust_root.json"
```

SchemaForge loads the snapshot once at startup and clones it into every `cosign-keyless` verifier in the policy. Missing or malformed bundles fail loud; silent fallback to the embedded snapshot would hide rotation drift. Refresh on a documented cadence; pin it to the accreditation review schedule.

## Pitfalls

| Pitfall | Fix |
|---|---|
| Schema directory has an unsigned `.schema` (or a `.schema` missing from the manifest) | `schemaforge sign schemas/` to regenerate both `.sig` and manifest in one pass |
| Manifest signature is valid but a `.schema` hash differs from `manifest[file].sha256` | File was edited after signing — re-sign or revert the edit |
| `mode = "enforce"` + `--no-verify` in production | Refused unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set. Don't set this in steady-state CI; it exists for one-off recovery only |
| `cosign-keyless` in an airgap with no `trust_root_bundle` set | Fetch with `trust-bundle refresh` on a connected host, transfer, point `trust_root_bundle` at the file |
| `trust_root_bundle` path missing or malformed | Verifier fails loud at startup. Silent fallback to the embedded snapshot would hide rotation drift, which is the whole reason this knob exists |
| SSH key works under `git config gpg.ssh.allowedSignersFile` but fails SchemaForge | Add `namespaces="schema-forge-signing@govcraft.ai"` to the allowed-signers line — SchemaForge signs under a dedicated namespace and a missing/wrong one rejects at the policy layer |
| Rotated cosign signing identity but verifier still rejects | Update `subject_pattern` in the trust policy. Fulcio certs are short-lived and need no manual rotation; the glob is the rotation surface |
