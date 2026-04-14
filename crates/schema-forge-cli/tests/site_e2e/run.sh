#!/usr/bin/env bash
# Orchestrator for the generated-site Playwright smoke suite.
#
# Boots a fresh schemaforge backend against an in-memory SurrealDB, generates
# a React site from tests/site_e2e/demo.schema, starts the Vite dev server,
# and runs the Playwright spec at tests/site_e2e/playwright/tests/smoke.spec.ts.
# Tears down both child processes on exit.
#
# Intended to run both locally and from .github/workflows/site-e2e.yml. Keep
# the script POSIX-friendly-ish but assume bash is available (CI images all
# have it).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

cd "$REPO_ROOT"

TMP_ROOT="${TMP_ROOT:-$REPO_ROOT/target/site-e2e-$$}"
SCHEMAS_DIR="$TMP_ROOT/schemas"
SITE_DIR="$TMP_ROOT/site"

mkdir -p "$SCHEMAS_DIR"
cp "$SCRIPT_DIR/demo.schema" "$SCHEMAS_DIR/demo.schema"

# ---------- pick two free ports ----------
pick_port() {
  python3 -c 'import socket,sys; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'
}
BACKEND_PORT="${BACKEND_PORT:-$(pick_port)}"
VITE_PORT="${VITE_PORT:-$(pick_port)}"

echo "== site-e2e ==============================================="
echo "  tmp:     $TMP_ROOT"
echo "  backend: http://127.0.0.1:$BACKEND_PORT"
echo "  vite:    http://127.0.0.1:$VITE_PORT"
echo "==========================================================="

# ---------- build the CLI ----------
cargo build --bin schemaforge --quiet

# ---------- generate the site ----------
./target/debug/schemaforge site generate \
  --schema-dir "$SCHEMAS_DIR" \
  --out-dir "$SITE_DIR"

# ---------- backend ----------
BACKEND_LOG="$TMP_ROOT/backend.log"
FORGE_ADMIN_USER=admin FORGE_ADMIN_PASSWORD=admin \
  ./target/debug/schemaforge serve \
    --db-url mem:// \
    --schemas "$SCHEMAS_DIR" \
    -H 127.0.0.1 \
    -p "$BACKEND_PORT" \
    >"$BACKEND_LOG" 2>&1 &
BACKEND_PID=$!

cleanup() {
  status=$?
  set +e
  if [[ -n "${VITE_PID:-}" ]] && kill -0 "$VITE_PID" 2>/dev/null; then
    kill "$VITE_PID" 2>/dev/null || true
    wait "$VITE_PID" 2>/dev/null || true
  fi
  if [[ -n "${BACKEND_PID:-}" ]] && kill -0 "$BACKEND_PID" 2>/dev/null; then
    kill "$BACKEND_PID" 2>/dev/null || true
    wait "$BACKEND_PID" 2>/dev/null || true
  fi
  if [[ $status -ne 0 ]]; then
    echo "--- backend log ---"
    tail -100 "$BACKEND_LOG" 2>/dev/null || true
    echo "--- vite log ---"
    tail -100 "$TMP_ROOT/vite.log" 2>/dev/null || true
  fi
  exit $status
}
trap cleanup EXIT INT TERM

wait_for_http() {
  local url="$1" timeout="${2:-60}" i=0
  until curl -sf "$url" >/dev/null 2>&1; do
    i=$((i + 1))
    if [[ $i -ge $timeout ]]; then
      echo "timeout waiting for $url" >&2
      return 1
    fi
    sleep 1
  done
}

wait_for_http "http://127.0.0.1:$BACKEND_PORT/health"

# ---------- site (install + dev) ----------
pushd "$SITE_DIR" >/dev/null
if [[ ! -d node_modules ]]; then
  pnpm install --frozen-lockfile 2>&1 | tail -5 || pnpm install 2>&1 | tail -5
fi

VITE_LOG="$TMP_ROOT/vite.log"
VITE_FORGE_UPSTREAM="http://127.0.0.1:$BACKEND_PORT" \
  pnpm exec vite --host 127.0.0.1 --port "$VITE_PORT" >"$VITE_LOG" 2>&1 &
VITE_PID=$!
popd >/dev/null

wait_for_http "http://127.0.0.1:$VITE_PORT/"

# ---------- playwright ----------
PLAYWRIGHT_DIR="$SCRIPT_DIR/playwright"
pushd "$PLAYWRIGHT_DIR" >/dev/null
if [[ ! -d node_modules ]]; then
  pnpm install --frozen-lockfile 2>&1 | tail -5 || pnpm install 2>&1 | tail -5
fi
pnpm exec playwright install --with-deps chromium 2>&1 | tail -3 || true
BASE_URL="http://127.0.0.1:$VITE_PORT" pnpm exec playwright test
popd >/dev/null

echo "site-e2e: all checks passed"
