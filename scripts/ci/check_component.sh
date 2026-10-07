#!/usr/bin/env bash
# Keep the package/feature graph identical across separately visible CI steps.
set -euo pipefail

features=()
case "${COMPONENT:?COMPONENT is required}" in
  tooling)
    packages=(-p schema-forge-core -p schema-forge-cel -p schema-forge-dsl -p schema-forge-config -p schema-forge-codegen -p schema-forge-signing -p schema-forge-cli)
    ;;
  cli)
    packages=(-p schema-forge-cli)
    features=(--features "server,oauth,sse")
    ;;
  runtime)
    packages=(-p schema-forge-acton -p schema-forge-backend)
    ;;
  *) echo "Unknown component: $COMPONENT" >&2; exit 1 ;;
esac

case "${1:?Expected tests, lint, or doctests}" in
  tests) cargo nextest run --locked "${packages[@]}" --no-default-features "${features[@]}" --no-fail-fast ;;
  lint) cargo clippy --locked "${packages[@]}" --no-default-features "${features[@]}" --all-targets -- -D warnings ;;
  doctests)
    if [[ "$COMPONENT" != tooling ]]; then echo "Doctests belong to tooling" >&2; exit 1; fi
    cargo test --locked "${packages[@]}" --no-default-features --doc
    ;;
  *) echo "Unknown component check: $1" >&2; exit 1 ;;
esac
