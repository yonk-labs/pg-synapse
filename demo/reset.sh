#!/usr/bin/env bash
# Reset the pg-one demo stack to a clean, ready-to-test state.
#
# `docker compose down -v` wipes the Postgres data volume (needed after any
# schema/tool change, since --force-recreate alone does not touch it), and
# the harness never auto-seeds an LLM profile on a fresh volume: the UI only
# calls synapse.llm_profile_set() when a human clicks Save in Panel 1. Every
# fresh volume otherwise starts with zero usable agents. This script does
# both steps so a test run can start immediately after.
#
# Usage: demo/reset.sh
# Set your own endpoint: DEFAULT_LLM_BASE_URL=... DEFAULT_LLM_MODEL=... demo/reset.sh
# (or drop a local, gitignored .env in the repo root with the same names)

set -euo pipefail
cd "$(dirname "$0")/.."

LLM_BASE_URL="${DEFAULT_LLM_BASE_URL:-http://localhost:8000/v1}"
LLM_MODEL="${DEFAULT_LLM_MODEL:-local-model}"

echo "==> docker compose down -v"
docker compose down -v

echo "==> docker compose up -d"
docker compose up -d

echo "==> waiting for the harness to answer"
HARNESS_PORT="$(docker compose port harness 8080 | cut -d: -f2)"
HARNESS_URL="http://localhost:${HARNESS_PORT}"
for _ in $(seq 1 60); do
  if curl -sf "${HARNESS_URL}/" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "==> seeding vllm-default: ${LLM_MODEL} @ ${LLM_BASE_URL}"
curl -sf -X POST "${HARNESS_URL}/api/profile" \
  -H 'content-type: application/json' \
  -d "{\"base_url\":\"${LLM_BASE_URL}\",\"model\":\"${LLM_MODEL}\"}" \
  && echo
echo "==> ready: ${HARNESS_URL}"
