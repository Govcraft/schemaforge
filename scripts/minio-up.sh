#!/usr/bin/env bash
# Start MinIO for schema-forge-acton's `file_field_e2e` test and print the
# environment variables the test reads. Runs from any cwd.
#
#   usage: source scripts/minio-up.sh
#
# Idempotent: if MinIO is already running under the compose project it will be
# left in place and the env vars re-exported.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.minio.yml"

docker compose -f "${COMPOSE_FILE}" up -d --wait

export SCHEMAFORGE_E2E_S3_ENDPOINT="http://127.0.0.1:9100"
export SCHEMAFORGE_E2E_S3_ACCESS_KEY="minioadmin"
export SCHEMAFORGE_E2E_S3_SECRET_KEY="minioadmin"
export SCHEMAFORGE_E2E_S3_BUCKET="forge-e2e"
export SCHEMAFORGE_E2E_S3_REGION="us-east-1"

echo "MinIO ready at ${SCHEMAFORGE_E2E_S3_ENDPOINT} (bucket: ${SCHEMAFORGE_E2E_S3_BUCKET})."
echo "Console: http://127.0.0.1:9101 (minioadmin / minioadmin)"
echo
echo "Run the e2e test with:"
echo "  cargo nextest run --run-ignored all -p schema-forge-acton --test file_field_e2e"
