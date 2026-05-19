# Changelog

All notable changes to SchemaForge are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project
is pre-1.0; breaking changes bump the **minor** version per
[SemVer](https://semver.org/#spec-item-4) for the `0.y.z` series.

## [Unreleased]

### Added

- **Signed-schema enforcement.** New `schema-forge-signing` crate verifies
  per-file digital signatures and a signed directory manifest before any
  `.schema` file is parsed by `apply`, `migrate`, `serve`, `parse`,
  `export`, `policies`, `hooks`, or `site`. The trust policy lives under
  `[schema_forge.signing]` in `config.toml`; three signer kinds are
  defined — `ed25519` (Phase 1, shipped), `ssh-allowed-signers` (Phase 2,
  shipped), and `cosign-keyless` (Phase 3, shipped). Trust evaluation uses
  OR-semantics so rotating keys is additive. Three modes: `off` (default
  for now, preserves pre-signing behaviour), `warn` (run checks, log
  failures, continue), `enforce` (any failure aborts with exit code 13).
  Two new subcommands wrap the verifier:
    - `schemaforge sign <paths>` — produce per-file `.sig` files and a
      signed `schemas.manifest.toml`. `--ed25519-generate` creates a
      fresh keypair; `--ed25519-key` reuses one; `--ssh-key` signs with
      an existing OpenSSH private key (SSHSIG format, identical to
      `ssh-keygen -Y sign`); `--keyless` shells out to `cosign
      sign-blob --bundle …` so the on-disk `.sig` becomes a Sigstore
      Bundle JSON ready for offline verification; `--print-pubkey`
      emits the trust-anchor block matching the chosen scheme.
      `--ssh-principal <id>` customises the principal label printed in
      the allowed-signers advisory output. `--cosign-bin <path>`
      overrides the `cosign` binary location used by `--keyless`.
    - `schemaforge verify <paths>` — standalone verifier suitable as a
      pre-merge CI gate; touches no database.

  Two new global flags route through the verifier:
    - `--trust-policy <path>` overrides `[schema_forge.signing]` with a
      standalone TOML, so one shared `config.toml` can fan out to many
      environments without duplicating database settings.
    - `--no-verify` skips verification for one invocation, but is
      *refused* when `signing.mode = "enforce"` unless
      `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set — production deployments
      cannot silently skip verification.

  Defeats two threat classes that an unsigned schema directory leaves
  open: (1) filesystem-level tampering of any `.schema`, (2) introduction
  of untrusted authors via "drop a file in `schemas/`." Per-file
  detached signatures cover tampering; the signed manifest with pinned
  SHA-256s and an explicit file list covers add/remove attacks.

  Phase 2 adds the **SSH allowed_signers** verifier: trust roots can now
  point at an `allowed_signers` file (the same format `git config
  gpg.ssh.allowedSignersFile` consumes), and signatures live as
  PEM-armored SSHSIG blobs under namespace
  `schema-forge-signing@govcraft.ai`. Supports the
  `namespaces="..."`, `valid-after`, and `valid-before` per-line options,
  so a key rotated out of date or restricted to a different namespace is
  rejected at the policy layer before any cryptographic check runs.

  Phase 3 adds the **cosign-keyless** verifier so the same CI identity
  that already signs SchemaForge releases can sign schemas. Trust roots
  point at an OIDC `issuer` plus a glob `subject_pattern`; verification
  rides the `sigstore-verify` crate's full chain (cert ↔ Sigstore Fulcio
  root, SCT, Rekor inclusion proof, signature) and then post-checks the
  cert's OIDC subject against the operator glob. On disk, the `.sig`
  next to each schema is a Sigstore Bundle (`mediaType
  application/vnd.dev.sigstore.bundle.v0.3+json`) rather than the legacy
  `.sig`+`.pem` pair — bundles embed the Rekor inclusion proof, which
  preserves the historical signing time the (10-minute) Fulcio cert
  needs to validate long after expiry. Signing uses `schemaforge sign
  --keyless`, which shells out to `cosign sign-blob --yes --bundle`; we
  do not reimplement the Fulcio/Rekor OIDC dance because `cosign` is the
  canonical CLI for that flow and ships in every Sigstore-enabled CI
  environment. A new `--cosign-bin` overrides the binary location for
  runners with a non-standard install.

- New `fips` cargo feature on `schema-forge-cli` (and `schema-forge-acton`)
  routes rustls through `aws-lc-rs` compiled against the FIPS-validated
  AWS-LC C library (`aws-lc-fips-sys`). At startup, the CLI installs
  `aws_lc_rs` as the process-wide rustls `CryptoProvider`, so PostgreSQL
  (sqlx), S3 (reqwest), and the hook dispatcher (tonic) all terminate
  TLS through the FIPS module. Pair with `postgres`; the `surrealdb`
  backend still pulls `rustls/ring` transitively and is not FIPS-clean.
  Build requires CMake, a C/C++ toolchain, and Go 1.18+. See README
  "FIPS builds".

### Changed

- Upgraded `acton-service` to **0.26** in `schema-forge-acton`,
  `schema-forge-cli`, and `schema-forge-backend`. 0.26's new
  `crypto-aws-lc-rs` feature (enabled by default in our build) propagates
  `aws-lc-rs` through `rustls`, `tokio-rustls`, `reqwest`, `sqlx`, and
  `tonic`, replacing the previous ring-backed default.
- `schema-forge-postgres` no longer pins `sqlx`'s `tls-rustls` (ring)
  feature; `acton-service`'s crypto feature drives the TLS provider so
  the FIPS path can swap cleanly.

### Security / Breaking

- `schemaforge serve` no longer auto-seeds the five SchemaForge demo personas
  (alice, bob, charlie, dana, eve — each with the literal password
  `"password"`) when bootstrapping the admin user via `--admin-user` /
  `--admin-password` (or `FORGE_ADMIN_USER` / `FORGE_ADMIN_PASSWORD`). The
  legacy behavior fired automatically whenever the user store was empty before
  bootstrap, leaving downstream AMI/customer deployments with six accounts and
  five default passwords. **Fixes [#53](https://github.com/govcraft/schemaforge/issues/53).**
- Demo persona seeding is now strictly opt-in via the new
  `--seed-demo-users` flag (env: `FORGE_SEED_DEMO_USERS`, default: `false`).
  The bundled `task demo` flow passes the flag explicitly; `task serve` does
  not.
- `schema_forge_acton::shared_auth::bootstrap_demo_users` gained a required
  `seed: bool` parameter. Existing callers in operator code must pass `false`;
  only controlled local-development flows should pass `true`.
- `SchemaForgeExtensionBuilder` gained `with_seed_demo_users(bool)`. The
  default is `false`, matching the new safe-by-construction posture.

### Migration

Operators upgrading from `schema-forge-cli` 0.27.x:

- If you were relying on the implicit demo seed for local development, add
  `--seed-demo-users` (or set `FORGE_SEED_DEMO_USERS=true`) to your serve
  invocation. The `task demo` Taskfile entry has already been updated.
- If your deployment was unintentionally inheriting the demo accounts, remove
  them with `schemaforge user delete <name>` (or your backend's equivalent) and
  rotate any passwords that may have leaked. They were created with the literal
  string `"password"`.

### Version bumps

- `schema-forge-cli`: 0.27.0 → 0.28.0
- `schema-forge-acton`: 0.26.0 → 0.27.0
