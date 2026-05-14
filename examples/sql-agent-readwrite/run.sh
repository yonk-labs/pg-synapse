#!/usr/bin/env bash
# Repeatable end-to-end demo: an agent reads + writes a Postgres table via SQL.
#
# What this does:
#   1. Starts the cargo-pgrx managed Postgres (pg17) if not already running.
#   2. Drops + recreates a clean `pg_synapse_demo` database.
#   3. Installs the pg_synapse_pgrx extension.
#   4. Applies seed.sql (creates demo.notes with 2 rows).
#   5. Applies workflow.sql (registers profile + agent, runs synapse.execute twice).
#   6. Prints the final state of demo.notes plus the execution + message logs.
#   7. Asserts that the agent grew demo.notes by at least one row.
#
# Prerequisites:
#   - cargo-pgrx 0.18+ installed (cargo install cargo-pgrx --version =0.18.0 --locked)
#   - pgrx init has been run (cargo pgrx init --pg17 download)
#   - The extension has been installed (cargo pgrx install ... or `cargo pgrx run pg17`
#     once, then exit; that places .so + .sql + .control under the pgrx pg17 tree)
#   - A reachable OpenAI-compatible LLM endpoint with tool-call support
#     (default: vLLM at $PG_SYNAPSE_LLM_BASE_URL)
#
# Configure via env vars:
#   PG_SYNAPSE_LLM_BASE_URL  default: http://192.168.1.193:8000/v1
#   PG_SYNAPSE_LLM_MODEL     default: Intel/Qwen3-Coder-Next-int4-AutoRound
#   PGRX_PG_VERSION          default: 17
#   PGRX_PORT                default: 28817 (pgrx-0.18 default for pg17)
#   PGRX_HOST                default: /home/$USER/.pgrx (pgrx uses its own dir as
#                            the unix_socket_directories value, NOT /tmp)
#   DEMO_DB                  default: pg_synapse_demo

set -euo pipefail

# v0.1: docker path is not yet wired (see README "Path B"). Honor --docker only
# to print a clear note and exit non-zero so CI does not silently pass.
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
DEMO_DB="${DEMO_DB:-pg_synapse_demo}"

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

INITIAL_COUNT=$(${DEMO_PSQL} -tA -c "SELECT count(*) FROM demo.notes;")
echo ">>> Initial demo.notes (${INITIAL_COUNT} rows):"
${DEMO_PSQL} -c "SELECT id, body, added_by FROM demo.notes ORDER BY id;"

# Allow env-var override of LLM endpoint + model without editing workflow.sql.
WF_TMP="$(mktemp)"
trap 'rm -f "${WF_TMP}"' EXIT
sed \
  -e "s|http://192.168.1.193:8000/v1|${LLM_BASE_URL}|g" \
  -e "s|Intel/Qwen3-Coder-Next-int4-AutoRound|${LLM_MODEL}|g" \
  "${WORKFLOW}" > "${WF_TMP}"

echo ""
echo ">>> Running agent workflow"
echo ">>>   endpoint: ${LLM_BASE_URL}"
echo ">>>   model:    ${LLM_MODEL}"
${DEMO_PSQL} -f "${WF_TMP}"

FINAL_COUNT=$(${DEMO_PSQL} -tA -c "SELECT count(*) FROM demo.notes;")

echo ""
echo ">>> Final demo.notes (${FINAL_COUNT} rows):"
${DEMO_PSQL} -c "SELECT id, body, added_by FROM demo.notes ORDER BY id;"

echo ""
echo ">>> synapse.executions:"
${DEMO_PSQL} -c "SELECT execution_id, agent_name, status, tokens_in, tokens_out, duration_ms FROM synapse.executions ORDER BY started_at;"

echo ""
echo ">>> synapse.messages (role / tool_name / content preview):"
${DEMO_PSQL} -c "SELECT execution_id, seq, role, tool_name, LEFT(COALESCE(content, ''), 80) AS content_preview FROM synapse.messages ORDER BY execution_id, seq;"

echo ""
if [[ "${FINAL_COUNT}" -gt "${INITIAL_COUNT}" ]]; then
  echo "SUCCESS: demo.notes grew from ${INITIAL_COUNT} to ${FINAL_COUNT} rows."
  exit 0
else
  echo "FAILURE: demo.notes did not grow (still ${FINAL_COUNT} rows)." >&2
  echo "Inspect synapse.messages above for the model's behavior." >&2
  exit 1
fi
