#!/usr/bin/env bash
# Repeatable end-to-end demo of synapse.embed() against the local ORT-backed
# BGE-small embedding model.
#
# What this does:
#   1. Verifies ORT_DYLIB_PATH is set and the library exists (pgrx postmaster
#      must have inherited it; see notes below).
#   2. Starts the cargo-pgrx managed Postgres (pg17) if not running.
#   3. Drops + recreates a clean `pg_synapse_embed` database.
#   4. Installs the pg_synapse_pgrx extension.
#   5. Applies seed.sql (creates demo.snippets).
#   6. Applies workflow.sql (registers BGE-small profile, embeds 3 snippets,
#      ranks them against a query).
#   7. Asserts that the "Cats and dogs..." snippet ranks first against the
#      pets query.
#
# About ORT_DYLIB_PATH:
#   The Postgres backend that loads the extension needs libonnxruntime 1.24.x
#   visible. Postgres does NOT read arbitrary env vars from psql sessions; the
#   variable must be set in the env of the postmaster process.
#
#   Easiest path:
#     # stop any running pgrx postgres first
#     cargo pgrx stop pg17
#     ORT_DYLIB_PATH=/path/to/libonnxruntime.so.1.24.x cargo pgrx start pg17
#     bash examples/with-local-embeddings/run.sh
#
#   This script does NOT restart the postmaster; if the embed() call fails
#   with "library not found" or "ORT_API_VERSION mismatch", restart with
#   ORT_DYLIB_PATH set per the snippet above.
#
# Configure via env vars:
#   PGRX_PG_VERSION   default: 17
#   PGRX_PORT         default: 28817
#   PGRX_HOST         default: /home/$USER/.pgrx
#   DEMO_DB           default: pg_synapse_embed
#   MODEL_DIR         default: ~/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5

set -euo pipefail

PG_VER="${PGRX_PG_VERSION:-17}"
PGRX_PORT="${PGRX_PORT:-28817}"
PGRX_HOST="${PGRX_HOST:-${HOME}/.pgrx}"
DEMO_DB="${DEMO_DB:-pg_synapse_embed}"
MODEL_DIR="${MODEL_DIR:-${HOME}/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SEED="${SCRIPT_DIR}/seed.sql"
WORKFLOW="${SCRIPT_DIR}/workflow.sql"

cd "${REPO_ROOT}"

if [[ ! -f "${MODEL_DIR}/model.onnx" || ! -f "${MODEL_DIR}/tokenizer.json" ]]; then
  echo "ERROR: BGE-small model files not found at ${MODEL_DIR}" >&2
  echo "Either re-run the ORT plugin's download path or set MODEL_DIR to point" >&2
  echo "at an existing model + tokenizer pair." >&2
  exit 2
fi

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

# Patch the model paths in workflow.sql if MODEL_DIR differs from the default.
WF_TMP="$(mktemp)"
trap 'rm -f "${WF_TMP}"' EXIT
sed \
  -e "s|/home/yonk/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5|${MODEL_DIR}|g" \
  "${WORKFLOW}" > "${WF_TMP}"

echo ""
echo ">>> Running embeddings workflow"
echo ">>>   model_dir: ${MODEL_DIR}"
${DEMO_PSQL} -f "${WF_TMP}"

# Pull the top-ranked text for the assertion below.
TOP_TEXT=$(${DEMO_PSQL} -tA <<'SQL'
WITH q AS (
  SELECT synapse.embed('What kind of pets do people keep at home?', 'bge-small') AS vec
)
SELECT s.text
FROM demo.snippets s, q
ORDER BY (SELECT sum(a*b) FROM unnest(s.embedding, q.vec) AS u(a, b)) DESC
LIMIT 1;
SQL
)

echo ""
echo ">>> Top-ranked snippet for the pets query: ${TOP_TEXT}"

if [[ "${TOP_TEXT}" == *"pets"* ]]; then
  echo "SUCCESS: pets snippet ranked first (BGE-small cosine ranking is working)."
  exit 0
else
  echo "FAILURE: expected the pets snippet to rank first, got: ${TOP_TEXT}" >&2
  exit 1
fi
