#!/usr/bin/env bash
# diag_one.sh: run ONE (model, scenario) cell exactly like run_bench.sh, but
# keep the database and dump the full step/decision trace from synapse.messages.
# No teardown. For root-cause analysis, not scoring.
set -uo pipefail

REPO_ROOT="/home/yonk/yonk-tools/pg-synapse"
BENCH="$REPO_ROOT/bench"
SCEN_NAME="${1:-a2_distill}"
DB="diag_${SCEN_NAME}"
SCEN_DIR="$BENCH/scenarios/$SCEN_NAME"

# --- model: vllm-qwen3-coder (remote, no auth) ---
PROV="openai"
MODEL_ID="Intel/Qwen3-Coder-Next-int4-AutoRound"
BASE_URL="http://192.168.1.193:8000/v1"

# --- PG connection (mirror run_bench.sh) ---
PG_PORT="28817"
PG_USER="$(whoami)"
_PGRX_DATA="/home/yonk/.pgrx/data-17"
if [[ -f "$_PGRX_DATA/postmaster.pid" ]]; then
  PG_SOCKET_DIR="$(sed -n '5p' "$_PGRX_DATA/postmaster.pid" | tr -d '[:space:]')"
  PG_SOCKET_DIR="${PG_SOCKET_DIR:-/home/yonk/.pgrx}"
else
  PG_SOCKET_DIR="/home/yonk/.pgrx"
fi
PSQL() { PGPASSWORD="" PGHOST="$PG_SOCKET_DIR" psql -p "$PG_PORT" -U "$PG_USER" "$@" 2>&1; }

# --- scenario config ---
source <(grep -E '^(KIND|TOOLS|MAX_ITER)=' "$SCEN_DIR/meta.env")
TOOLS="${TOOLS:-sql_query,sql_exec}"
MAX_ITER="${MAX_ITER:-25}"
TIMEOUT_MS=180000

echo "### DIAG: model=vllm-qwen3-coder scenario=$SCEN_NAME db=$DB tools=$TOOLS max_iter=$MAX_ITER"

# --- fresh DB ---
PSQL -d postgres -c "DROP DATABASE IF EXISTS \"$DB\";" >/dev/null 2>&1
PSQL -d postgres -c "CREATE DATABASE \"$DB\";" >/dev/null 2>&1
PSQL -d "$DB" -c "CREATE EXTENSION IF NOT EXISTS pg_synapse_pgrx;" >/dev/null 2>&1

# --- seed (render {{SCALE}}=1) ---
sed 's/{{SCALE}}/1/g' "$SCEN_DIR/seed.sql.tmpl" | PSQL -d "$DB" -f - >/dev/null 2>&1

# --- profile + agent (generate SQL via python to avoid shell-escaping) ---
SETUP_SQL="$(mktemp /tmp/diag_setup_XXXX.sql)"
python3 - "$SCEN_DIR/system_prompt.txt" "$PROV" "$MODEL_ID" "$BASE_URL" \
  "$TOOLS" "$MAX_ITER" "$TIMEOUT_MS" > "$SETUP_SQL" <<'PY'
import sys
sp_file, prov, model_id, base_url, tools, max_iter, timeout_ms = sys.argv[1:8]
sp = open(sp_file).read()
tool_arr = "ARRAY[" + ",".join("'%s'" % t.strip() for t in tools.split(",")) + "]"
print("SELECT synapse.llm_profile_set('bench_profile','%s','%s','%s',NULL,'{}'::jsonb);"
      % (prov, model_id, base_url))
print("SELECT synapse.agent_create('bench_agent',$SP$%s$SP$,'conversation','bench_profile',%s,%s,%s);"
      % (sp, tool_arr, max_iter, timeout_ms))
PY
PSQL -d "$DB" -f "$SETUP_SQL" >/dev/null 2>&1
rm -f "$SETUP_SQL"

# --- execute ---
TASK="$(sed 's/{{SCALE}}/1/g' "$SCEN_DIR/task.txt")"
TASK_SQL="$(mktemp /tmp/diag_task_XXXX.sql)"
python3 - "$TASK" "$TIMEOUT_MS" > "$TASK_SQL" <<'PY'
import sys
task, tmo = sys.argv[1], sys.argv[2]
print("SET statement_timeout = %s;" % tmo)
print("SELECT synapse.execute('bench_agent',$TK$%s$TK$);" % task)
PY
echo "### EXECUTE (this calls the live model; may take 10-120s) ..."
PSQL -d "$DB" -Atf "$TASK_SQL"
rm -f "$TASK_SQL"

echo
echo "### EXECUTION ROW"
PSQL -d "$DB" -x -c "SELECT execution_id, status, tokens_in, tokens_out, duration_ms, left(output,400) AS output_head FROM synapse.executions ORDER BY started_at DESC LIMIT 1;"

echo
echo "### ASSERT RESULT (did it really do what we wanted?)"
PSQL -d "$DB" -Atc "$(cat "$SCEN_DIR/assert.sql")"

echo
echo "### FULL STEP/DECISION TRACE (synapse.messages)"
PSQL -d "$DB" -x -c "
SELECT seq, role, tool_name,
       left(coalesce(content,''),600)        AS content,
       left(coalesce(tool_input::text,''),600)  AS tool_input,
       left(coalesce(tool_output::text,''),600) AS tool_output
FROM synapse.messages
WHERE execution_id = (SELECT execution_id FROM synapse.executions ORDER BY started_at DESC LIMIT 1)
ORDER BY seq;"

echo
echo "### TARGET TABLE STATE AFTER RUN"
case "$SCEN_NAME" in
  a2_distill) PSQL -d "$DB" -c "SELECT id, sentiment, left(gist,80) gist FROM feedback.digest ORDER BY id;" ;;
  a1_ingest)  PSQL -d "$DB" -c "SELECT 'customers' t, count(*) FROM ingest.customers UNION ALL SELECT 'orders', count(*) FROM ingest.orders;" ;;
esac
echo "### DB '$DB' KEPT for inspection."
