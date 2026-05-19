# Cosign-keyless verifier fixtures

Source: `sigstore-verify` v0.7.0 (Apache-2.0), `test_data/bundles/`.

- `cosign-v3-blob.txt` — the 24-byte artifact (`test content for cosign\n`)
  that was signed by `cosign sign-blob --bundle …` against the
  Sigstore production instance.
- `cosign-v3-blob.sigstore.json` — the matching Sigstore Bundle v0.3
  artifact, containing a short-lived Fulcio cert, the detached
  ECDSA-P256 signature, and the Rekor inclusion proof.

The bundle's OIDC identity is an email under issuer
`https://github.com/login/oauth`, and the Rekor `integratedTime`
preserves the historical signing time so the (long-expired) Fulcio
cert still validates against the embedded production trust root.

- `trust_root_fixture.json` — the Sigstore public-good trust-root
  snapshot also borrowed from `sigstore-verify` v0.7.0
  (`test_data/trusted_roots/public-good.json`). Used by Phase 4
  tests to exercise the `trust_root_bundle` override path through
  `VerifyPolicy::from_config` without depending on the embedded
  snapshot inside `sigstore-trust-root` (which is opaque to us and
  could change with a crate bump).

These fixtures are deterministic and require no network. Tests in
`src/verifiers/cosign.rs` and `src/policy.rs` reference them with
`include_bytes!` / `include_str!`.
