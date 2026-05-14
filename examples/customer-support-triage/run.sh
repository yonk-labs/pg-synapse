#!/usr/bin/env bash
# Repeatable end-to-end demo: a triage agent classifies three support tickets,
# joins each one with its customer record, and writes back category, priority,
# and an escalated flag.
#
# What this does:
#   1. Starts the cargo-pgrx managed Postgres (pg17) if not already running.
#   2. Drops + recreates a clean `pg_synapse_triage` database.
#   3. Installs the pg_synapse_pgrx extension.
#   4. Applies seed.sql (customers + tickets, 3 each).
#   5. Applies workflow.sql (registers profile + agent, runs synapse.execute thrice).
#   6. Prints the final state of support.tickets plus execution + message logs.
#   7. Asserts every ticket has a non-null category and priority.
#
# Prerequisites:
#   - cargo-pgrx 0.18+ installed (cargo install cargo-pgrx --version =0.18.0 --locked)
#   - pgrx init has been run (cargo pgrx init --pg17 download)
#   - Extension installed (cargo pgrx install ... or `cargo pgrx run pg17` once)
#   - A reachable OpenAI-compatible LLM with tool-call support
#
# Configure via env vars:
#   PG_SYNAPSE_LLM_BASE_URL  default: http://192.168.1.193:8000/v1
#   PG_SYNAPSE_LLM_MODEL     default: Intel/Qwen3-Coder-Next-int4-AutoRound
#   PGRX_PG_VERSION          default: 17
#   PGRX_PORT                default: 28817
#   PGRX_HOST                default: /home/$USER/.pgrx (pgrx's unix_socket_directories)
#   DEMO_DB                  default: pg_synapse_triage

set -euo pipefail

if [[ "${1:-}" == "--docker" ]]; then
  echo "Docker harness is not yet implemented in v0.1-alpha." >&2
  echo "Use the cargo-pgrx path: re-run this script without --docker." >&2
  exit 2
fi

LLM_BASE_URL="${PG_SYNAPSE_LLM_BASE_URL:-http://192.168.1.193:8000/v1}"
LLM_MODEL="${PG_SYNAPSE_LLM_MODEL:-Intel/Qwen3-Coder-Next-int4-AutoRound}"
PG_VER="${PGRX_PG_VERSION:-17}"
PGRX_PORT="${PGRX_PORT:-28817}"
PGRX_HOST="${PGRX_HOST:-${HOME}/.pgrx}"
DEMO_DB="${DEMO_DB:-pg_synapse_triage}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SEED="${SCRIPT_DIR}/seed.sql"
WORKFLOW="${SCRIPT_DIR}/workflow.sql"

cd "${REPO_ROOT}"

echo ">>> Starting pgrx-managed Postgres ${PG_VER} (if not running)"
cargo pgrx start "pg${PG_VER}" 2>&1 | tail -3 || true

PSQL_BASE="psql -h ${PGRX_HOST} -p ${PGRX_PORT} -d postgres -v ON_ERROR_STOP=1"
DEMO_PSQL="psql -h ${PGRX_HOST} -p ${PGRX_PORT} -d ${DEMO_DB} -v ON_ERROR_STOP=1"

echo ">>> Verifying connection: ${PGRX_HOST}:${PGRX_PORT}"
${PSQL_BASE} -tA -c "SELECT 'ok';" >/dev/null

echo ">>> Resetting database: ${DEMO_DB}"
${PSQL_BASE} -c "DROP DATABASE IF EXISTS ${DEMO_DB};" >/dev/null
${PSQL_BASE} -c "CREATE DATABASE ${DEMO_DB};" >/dev/null

echo ">>> Installing extension: pg_synapse_pgrx"
${DEMO_PSQL} -c "CREATE EXTENSION pg_synapse_pgrx;" >/dev/null

echo ">>> Applying seed.sql"
${DEMO_PSQL} -f "${SEED}" >/dev/null

echo ">>> Initial support.tickets:"
${DEMO_PSQL} -c "SELECT id, subject, category, priority, escalated FROM support.tickets ORDER BY id;"

# Allow env-var override of LLM endpoint + model without editing workflow.sql.
WF_TMP="$(mktemp)"
trap 'rm -f "${WF_TMP}"' EXIT
sed \
  -e "s|http://192.168.1.193:8000/v1|${LLM_BASE_URL}|g" \
  -e "s|Intel/Qwen3-Coder-Next-int4-AutoRound|${LLM_MODEL}|g" \
  "${WORKFLOW}" > "${WF_TMP}"

echo ""
echo ">>> Running triage workflow"
echo ">>>   endpoint: ${LLM_BASE_URL}"
echo ">>>   model:    ${LLM_MODEL}"
${DEMO_PSQL} -f "${WF_TMP}"

echo ""
echo ">>> Final support.tickets:"
${DEMO_PSQL} -c "SELECT id, subject, category, priority, escalated FROM support.tickets ORDER BY id;"

echo ""
echo ">>> synapse.executions:"
${DEMO_PSQL} -c "SELECT agent_name, status, tokens_in, tokens_out, duration_ms FROM synapse.executions ORDER BY started_at;"

# Assertion: every ticket must have category + priority populated.
NULL_COUNT=$(${DEMO_PSQL} -tA -c "SELECT count(*) FROM support.tickets WHERE category IS NULL OR priority IS NULL;")
TOTAL=$(${DEMO_PSQL} -tA -c "SELECT count(*) FROM support.tickets;")

echo ""
if [[ "${NULL_COUNT}" -eq 0 && "${TOTAL}" -gt 0 ]]; then
  echo "SUCCESS: all ${TOTAL} tickets have category + priority assigned."
  exit 0
else
  echo "FAILURE: ${NULL_COUNT} of ${TOTAL} tickets still missing category or priority." >&2
  echo "Inspect synapse.messages above to see what the model did." >&2
  exit 1
fi
