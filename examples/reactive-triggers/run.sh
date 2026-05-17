#!/usr/bin/env bash
# Repeatable end-to-end demo for reactive triggers (queue mode + inline mode).
#
# What this does:
#   1. Starts the cargo-pgrx managed Postgres (pg17) if not already running.
#   2. Drops + recreates a clean pg_synapse_reactive database.
#   3. Installs the pg_synapse_pgrx extension.
#   4. Applies seed.sql (creates demo schema, tables, LLM profile, agents).
#   5. Runs queue_demo.sql: attach queue trigger, INSERT a ticket, drain queue,
#      show the agent-enriched result.
#   6. Runs inline_demo.sql: attach inline trigger, INSERT a bad order (expect
#      rollback with the agent reason), INSERT a good order (expect commit).
#
# Prerequisites:
#   - cargo-pgrx 0.18+ installed
#   - pgrx init has been run (cargo pgrx init --pg17 download)
#   - Extension installed (cargo pgrx install ... or cargo pgrx run pg17 once)
#   - A reachable OpenAI-compatible LLM with tool-call support
#
# Configure via env vars:
#   PG_SYNAPSE_LLM_BASE_URL  default: http://192.168.1.193:8000/v1
#   PG_SYNAPSE_LLM_MODEL     default: Intel/Qwen3-Coder-Next-int4-AutoRound
#   PGRX_PG_VERSION          default: 17
#   PGRX_PORT                default: 28817
#   PGRX_HOST                default: /home/$USER/.pgrx
#   DEMO_DB                  default: pg_synapse_reactive

set -euo pipefail

if [[ "${1:-}" == "--docker" ]]; then
  echo "Docker harness is not yet implemented. Use the cargo-pgrx path." >&2
  exit 2
fi

LLM_BASE_URL="${PG_SYNAPSE_LLM_BASE_URL:-http://192.168.1.193:8000/v1}"
LLM_MODEL="${PG_SYNAPSE_LLM_MODEL:-Intel/Qwen3-Coder-Next-int4-AutoRound}"
PG_VER="${PGRX_PG_VERSION:-17}"
PGRX_PORT="${PGRX_PORT:-28817}"
PGRX_HOST="${PGRX_HOST:-${HOME}/.pgrx}"
DEMO_DB="${DEMO_DB:-pg_synapse_reactive}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${REPO_ROOT}"

echo ">>> Starting pgrx-managed Postgres ${PG_VER} (if not running)"
cargo pgrx start "pg${PG_VER}" 2>&1 | tail -3 || true

PSQL_BASE="psql -h ${PGRX_HOST} -p ${PGRX_PORT} -d postgres -v ON_ERROR_STOP=1"
DEMO_PSQL="psql -h ${PGRX_HOST} -p ${PGRX_PORT} -d ${DEMO_DB} -v ON_ERROR_STOP=0"

echo ">>> Verifying connection: ${PGRX_HOST}:${PGRX_PORT}"
${PSQL_BASE} -tA -c "SELECT 'ok';" >/dev/null

echo ">>> Resetting database: ${DEMO_DB}"
${PSQL_BASE} -c "DROP DATABASE IF EXISTS ${DEMO_DB};" >/dev/null
${PSQL_BASE} -c "CREATE DATABASE ${DEMO_DB};" >/dev/null

echo ">>> Installing extension: pg_synapse_pgrx"
${DEMO_PSQL} -c "CREATE EXTENSION pg_synapse_pgrx;" >/dev/null

# Substitute LLM endpoint + model into seed.sql before applying.
SEED_TMP="$(mktemp)"
trap 'rm -f "${SEED_TMP}"' EXIT
sed \
  -e "s|http://192.168.1.193:8000/v1|${LLM_BASE_URL}|g" \
  -e "s|Intel/Qwen3-Coder-Next-int4-AutoRound|${LLM_MODEL}|g" \
  "${SCRIPT_DIR}/seed.sql" > "${SEED_TMP}"

echo ">>> Applying seed.sql"
echo ">>>   endpoint: ${LLM_BASE_URL}"
echo ">>>   model:    ${LLM_MODEL}"
${DEMO_PSQL} -f "${SEED_TMP}" >/dev/null

echo ""
echo "================================================================="
echo ">>> QUEUE MODE DEMO"
echo "================================================================="
${DEMO_PSQL} -f "${SCRIPT_DIR}/queue_demo.sql"

echo ""
echo "================================================================="
echo ">>> INLINE MODE DEMO"
echo "================================================================="
${DEMO_PSQL} -f "${SCRIPT_DIR}/inline_demo.sql"

echo ""
echo "================================================================="
echo "SUCCESS: reactive-triggers demo completed."
echo "================================================================="
