#!/usr/bin/env bash
# run_n.sh: run ONE (model, scenario) cell N times to characterize the
# stochastic failure rate and failure modes. Records one line per iteration.
# Not a score: a distribution + failure-mode tally to drive retry design.
set -uo pipefail

REPO_ROOT="/home/yonk/yonk-tools/pg-synapse"
BENCH="$REPO_ROOT/bench"
SCEN_NAME="${1:-a2_distill}"
N="${2:-10}"
SCEN_DIR="$BENCH/scenarios/$SCEN_NAME"
OUT="$BENCH/run_n_${SCEN_NAME}.log"
: > "$OUT"

PROV="openai"
MODEL_ID="Intel/Qwen3-Coder-Next-int4-AutoRound"
BASE_URL="http://192.168.1.193:8000/v1"

PG_PORT="28817"; PG_USER="$(whoami)"
_PGRX_DATA="/home/yonk/.pgrx/data-17"
if [[ -f "$_PGRX_DATA/postmaster.pid" ]]; then
  PG_SOCKET_DIR="$(sed -n '5p' "$_PGRX_DATA/postmaster.pid" | tr -d '[:space:]')"
  PG_SOCKET_DIR="${PG_SOCKET_DIR:-/home/yonk/.pgrx}"
else PG_SOCKET_DIR="/home/yonk/.pgrx"; fi
PSQL() { PGPASSWORD="" PGHOST="$PG_SOCKET_DIR" psql -p "$PG_PORT" -U "$PG_USER" "$@" 2>&1; }

source <(grep -E '^(KIND|TOOLS|MAX_ITER)=' "$SCEN_DIR/meta.env")
TOOLS="${TOOLS:-sql_query,sql_exec}"; MAX_ITER="${MAX_ITER:-25}"; TIMEOUT_MS=180000

log() { echo "$*" | tee -a "$OUT"; }
log "### run_n: vllm-qwen3-coder x $SCEN_NAME, N=$N, start=$(date -u +%H:%M:%S)Z"

PASS=0; FAIL=0
declare -A MODES
for i in $(seq 1 "$N"); do
  DB="runn_${SCEN_NAME}_$i"
  PSQL -d postgres -c "DROP DATABASE IF EXISTS \"$DB\";" >/dev/null 2>&1
  PSQL -d postgres -c "CREATE DATABASE \"$DB\";" >/dev/null 2>&1
  PSQL -d "$DB" -c "CREATE EXTENSION IF NOT EXISTS pg_synapse_pgrx;" >/dev/null 2>&1
  sed 's/{{SCALE}}/1/g' "$SCEN_DIR/seed.sql.tmpl" | PSQL -d "$DB" -f - >/dev/null 2>&1

  S="$(mktemp /tmp/rn_s_XXXX.sql)"
  python3 - "$SCEN_DIR/system_prompt.txt" "$PROV" "$MODEL_ID" "$BASE_URL" "$TOOLS" "$MAX_ITER" "$TIMEOUT_MS" > "$S" <<'PY'
import sys
sp_file, prov, mid, url, tools, mi, tmo = sys.argv[1:8]
sp = open(sp_file).read()
arr = "ARRAY[" + ",".join("'%s'" % t.strip() for t in tools.split(",")) + "]"
print("SELECT synapse.llm_profile_set('bench_profile','%s','%s','%s',NULL,'{}'::jsonb);" % (prov,mid,url))
print("SELECT synapse.agent_create('bench_agent',$SP$%s$SP$,'conversation','bench_profile',%s,%s,%s);" % (sp,arr,mi,tmo))
PY
  PSQL -d "$DB" -f "$S" >/dev/null 2>&1; rm -f "$S"

  TASK="$(sed 's/{{SCALE}}/1/g' "$SCEN_DIR/task.txt")"
  T="$(mktemp /tmp/rn_t_XXXX.sql)"
  python3 - "$TASK" "$TIMEOUT_MS" > "$T" <<'PY'
import sys
print("SET statement_timeout = %s;" % sys.argv[2])
print("SELECT synapse.execute('bench_agent',$TK$%s$TK$);" % sys.argv[1])
PY
  EXEC="$(PSQL -d "$DB" -Atf "$T" 2>&1)"; rm -f "$T"

  STATUS="$(echo "$EXEC" | python3 -c "import sys,json
for l in sys.stdin:
 l=l.strip()
 if l.startswith('{'):
  try: print(json.loads(l).get('status','?')); break
  except: pass" 2>/dev/null || echo '?')"
  ERR="$(echo "$EXEC" | python3 -c "import sys,json
for l in sys.stdin:
 l=l.strip()
 if l.startswith('{'):
  try: print(json.loads(l).get('error','')[:160]); break
  except: pass" 2>/dev/null || echo '')"
  ASSERT="$(PSQL -d "$DB" -Atc "$(cat "$SCEN_DIR/assert.sql")" 2>/dev/null || echo 'false')"
  GOOD_EXEC="$(PSQL -d "$DB" -Atc "SELECT count(*) FROM synapse.messages WHERE role='tool' AND tool_name='sql_exec' AND tool_output::text LIKE '%rows_affected%';" 2>/dev/null || echo 0)"

  if [[ "$ASSERT" == "t" ]]; then
    PASS=$((PASS+1)); VERDICT="PASS"; MODE="ok"
  else
    FAIL=$((FAIL+1)); VERDICT="FAIL"
    if   echo "$ERR" | grep -qi 'specified more than once'; then MODE="dup_column_slip"
    elif echo "$ERR" | grep -qi 'missing field'; then            MODE="bad_tool_arg_key"
    elif echo "$ERR" | grep -qi 'syntax error'; then              MODE="sql_syntax_slip"
    elif echo "$ERR" | grep -qi 'duplicate key'; then             MODE="duplicate_insert"
    elif echo "$ERR" | grep -qi 'timeout\|statement timeout'; then MODE="timeout"
    elif [[ -z "$ERR" ]]; then                                    MODE="completed_wrong_output"
    else                                                          MODE="other"; fi
  fi
  MODES[$MODE]=$(( ${MODES[$MODE]:-0} + 1 ))
  log "iter $i: $VERDICT  status=$STATUS  good_sql_exec=$GOOD_EXEC  mode=$MODE  err=${ERR:0:120}"
  PSQL -d postgres -c "DROP DATABASE IF EXISTS \"$DB\";" >/dev/null 2>&1
done

log ""
log "### SUMMARY  pass=$PASS  fail=$FAIL  (N=$N)"
for m in "${!MODES[@]}"; do log "  mode[$m] = ${MODES[$m]}"; done
log "### done $(date -u +%H:%M:%S)Z"
