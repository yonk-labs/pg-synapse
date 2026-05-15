#!/usr/bin/env bash
# Repeatable end-to-end demo: an agent reads + writes a Postgres table via SQL,
# using a local llama-server (llama.cpp) as the LLM backend.
#
# What this does:
#   1. Checks that llama-server is on PATH; if not, prints install hint and
#      exits 0 with a SKIP message.
#   2. Downloads the granite-3.0-2b-instruct Q4_K_M GGUF into
#      ~/.cache/pg-synapse/models/ if not already present (curl, idempotent).
#   3. Starts llama-server on a free port (found via Python socket trick).
#   4. Starts the cargo-pgrx managed Postgres (pg17) if not already running.
#   5. Drops + recreates a clean pg_synapse_demo database.
#   6. Installs the pg_synapse_pgrx extension.
#   7. Applies seed.sql (creates demo.tasks with 2 rows).
#   8. Applies workflow.sql (registers llama-cpp profile + agent, calls
#      synapse.execute twice).
#   9. Prints the final demo.tasks, synapse.executions, synapse.messages.
#  10. Asserts that the agent grew demo.tasks by at least one row.
#  11. Kills llama-server and drops the database.
#
# Prerequisites:
#   - cargo-pgrx 0.18+ installed (cargo install cargo-pgrx --version =0.18.0 --locked)
#   - pgrx init has been run (cargo pgrx init --pg17 download)
#   - The extension has been installed into the pgrx pg17 tree (run
#     cargo pgrx run pg17 once, then \q)
#   - curl available (for GGUF download)
#   - python3 available (for free-port detection)
#   - llama-server available on PATH (optional: script SKIPs if absent)
#
# Configure via env vars:
#   LLAMA_BASE_MODEL_URL   HuggingFace GGUF URL (default: granite Q4_K_M)
#   LLAMA_MODEL_CACHE      Local model cache dir (default: ~/.cache/pg-synapse/models)
#   LLAMA_MODEL_NAME       File name for the GGUF (default: auto from URL)
#   PGRX_PG_VERSION        default: 17
#   PGRX_PORT              default: 28817
#   PGRX_HOST              default: /home/$USER/.pgrx
#   DEMO_DB                default: pg_synapse_demo

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

HF_REPO="${LLAMA_HF_REPO:-lmstudio-community/granite-3.0-2b-instruct-GGUF}"
HF_FILE="${LLAMA_HF_FILE:-granite-3.0-2b-instruct-Q4_K_M.gguf}"
HF_REV="${LLAMA_HF_REV:-main}"

MODEL_CACHE="${LLAMA_MODEL_CACHE:-${HOME}/.cache/pg-synapse/models}"
MODEL_DIR="${MODEL_CACHE}/${HF_REPO}"
MODEL_PATH="${MODEL_DIR}/${HF_FILE}"

PG_VER="${PGRX_PG_VERSION:-17}"
PGRX_PORT="${PGRX_PORT:-28817}"
PGRX_HOST="${PGRX_HOST:-${HOME}/.pgrx}"
DEMO_DB="${DEMO_DB:-pg_synapse_demo}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# ---------------------------------------------------------------------------
# 1. Check llama-server availability (SKIP rather than fail if absent)
# ---------------------------------------------------------------------------

if ! command -v llama-server >/dev/null 2>&1; then
  echo ""
  echo "SKIP: llama-server not found on PATH."
  echo ""
  echo "Install llama.cpp server using one of these methods:"
  echo "  Homebrew (macOS):  brew install llama.cpp"
  echo "  Pre-built binary:  https://github.com/ggml-org/llama.cpp/releases"
  echo "  From source:       git clone https://github.com/ggml-org/llama.cpp"
  echo "                     cd llama.cpp && cmake -B build && cmake --build build"
  echo "                     # binary at: build/bin/llama-server"
  echo ""
  echo "Then re-run this script. No Postgres state was modified."
  exit 0
fi

echo ">>> llama-server found: $(command -v llama-server)"

# ---------------------------------------------------------------------------
# 2. Download GGUF (idempotent via curl; skip if already cached)
# ---------------------------------------------------------------------------

HF_URL="https://huggingface.co/${HF_REPO}/resolve/${HF_REV}/${HF_FILE}"

if [[ -f "${MODEL_PATH}" ]]; then
  echo ">>> GGUF already cached: ${MODEL_PATH}"
else
  echo ">>> Downloading GGUF from Hugging Face"
  echo ">>>   URL:  ${HF_URL}"
  echo ">>>   dest: ${MODEL_PATH}"
  mkdir -p "${MODEL_DIR}"
  TMP_PATH="${MODEL_PATH}.tmp"
  curl -L --progress-bar -o "${TMP_PATH}" "${HF_URL}"
  mv "${TMP_PATH}" "${MODEL_PATH}"
  echo ">>> Download complete: ${MODEL_PATH}"
fi

# ---------------------------------------------------------------------------
# 3. Find a free port and start llama-server
# ---------------------------------------------------------------------------

LLAMA_PORT=$(python3 -c "
import socket
s = socket.socket()
s.bind(('', 0))
port = s.getsockname()[1]
s.close()
print(port)
")

echo ">>> Starting llama-server on port ${LLAMA_PORT}"

llama-server \
  --model "${MODEL_PATH}" \
  --port "${LLAMA_PORT}" \
  --host 127.0.0.1 \
  --ctx-size 4096 \
  --threads 4 \
  --log-disable \
  > /tmp/llama-server-demo.log 2>&1 &

LLAMA_PID=$!

# Cleanup on exit: kill llama-server regardless of success/failure.
cleanup() {
  echo ">>> Stopping llama-server (pid ${LLAMA_PID})"
  kill "${LLAMA_PID}" 2>/dev/null || true
  wait "${LLAMA_PID}" 2>/dev/null || true
}
trap cleanup EXIT

LLAMA_BASE_URL="http://127.0.0.1:${LLAMA_PORT}/v1"

# Wait up to 30 s for llama-server to become ready.
echo ">>> Waiting for llama-server to become ready..."
for i in $(seq 1 30); do
  if curl -sf "${LLAMA_BASE_URL}/models" >/dev/null 2>&1; then
    echo ">>> llama-server ready (took ${i}s)"
    break
  fi
  if [[ "${i}" -eq 30 ]]; then
    echo "ERROR: llama-server did not become ready in 30 s." >&2
    echo "Check /tmp/llama-server-demo.log for details." >&2
    exit 1
  fi
  sleep 1
done

# ---------------------------------------------------------------------------
# 4-7. Postgres setup
# ---------------------------------------------------------------------------

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
${DEMO_PSQL} -f "${SCRIPT_DIR}/seed.sql" >/dev/null

INITIAL_COUNT=$(${DEMO_PSQL} -tA -c "SELECT count(*) FROM demo.tasks;")
echo ">>> Initial demo.tasks (${INITIAL_COUNT} rows):"
${DEMO_PSQL} -c "SELECT id, title, status, added_by FROM demo.tasks ORDER BY id;"

# ---------------------------------------------------------------------------
# 8. Run workflow (patch base_url to the dynamically chosen port)
# ---------------------------------------------------------------------------

WF_TMP="$(mktemp)"
trap 'rm -f "${WF_TMP}"; cleanup' EXIT

sed \
  -e "s|http://127.0.0.1:8080/v1|${LLAMA_BASE_URL}|g" \
  "${SCRIPT_DIR}/workflow.sql" > "${WF_TMP}"

echo ""
echo ">>> Running agent workflow"
echo ">>>   endpoint: ${LLAMA_BASE_URL}"
echo ">>>   model:    ${HF_FILE}"
${DEMO_PSQL} -f "${WF_TMP}"

FINAL_COUNT=$(${DEMO_PSQL} -tA -c "SELECT count(*) FROM demo.tasks;")

echo ""
echo ">>> Final demo.tasks (${FINAL_COUNT} rows):"
${DEMO_PSQL} -c "SELECT id, title, status, added_by FROM demo.tasks ORDER BY id;"

echo ""
echo ">>> synapse.executions:"
${DEMO_PSQL} -c "SELECT execution_id, agent_name, status, tokens_in, tokens_out, duration_ms FROM synapse.executions ORDER BY started_at;"

echo ""
echo ">>> synapse.messages (role / tool_name / content preview):"
${DEMO_PSQL} -c "SELECT execution_id, seq, role, tool_name, LEFT(COALESCE(content, ''), 80) AS content_preview FROM synapse.messages ORDER BY execution_id, seq;"

echo ""
if [[ "${FINAL_COUNT}" -gt "${INITIAL_COUNT}" ]]; then
  echo "SUCCESS: demo.tasks grew from ${INITIAL_COUNT} to ${FINAL_COUNT} rows."
  exit 0
else
  echo "FAILURE: demo.tasks did not grow (still ${FINAL_COUNT} rows)." >&2
  echo "Inspect synapse.messages above for the model behavior." >&2
  exit 1
fi
