#!/usr/bin/env bash
# bench/run_bench.sh: pg_synapse model benchmark harness.
#
# Usage:
#   bench/run_bench.sh [OPTIONS]
#
# Options:
#   --models  k1,k2,...   comma-separated model keys (default: all in models.toml)
#   --scenarios s1,...    comma-separated scenario dirs (default: all in bench/scenarios/)
#   --scale N             multiplier passed as {{SCALE}} in seed templates (default: 1)
#   --timeout SEC         statement_timeout in seconds for the agent execute() call (default: 180)
#   --force               re-run (model,scenario,scale) combos already in results.jsonl
#
# Output:
#   bench/results.jsonl   append-only JSON lines (one per model+scenario run)
#   bench/RESULTS.md      leaderboard regenerated after every run
#
# Clean-room: reads only this repo's own files; no external or private sources.
# Secret rule: .openai key extracted at runtime via sed; never echoed or logged.
# EM-dash rule: no em-dashes in this file.

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
BENCH_DIR="$SCRIPT_DIR"
SCENARIOS_DIR="$BENCH_DIR/scenarios"
MODELS_TOML="$BENCH_DIR/models.toml"
RESULTS_JSONL="$BENCH_DIR/results.jsonl"
RESULTS_MD="$BENCH_DIR/RESULTS.md"
MODEL_CACHE_DIR="$HOME/.cache/pg-synapse/models/bench"
LLAMA_SERVER_BIN="/tmp/pgs-venv/bin/python3"
OPENAI_KEY_FILE="$REPO_ROOT/.openai"

PG_PORT="28817"
PG_USER="$(whoami)"
# Resolve the pgrx unix socket directory from postmaster.pid (line 5).
# Fall back to /home/yonk/.pgrx if the pid file is absent or unreadable.
_PGRX_DATA="/home/yonk/.pgrx/data-17"
if [[ -f "$_PGRX_DATA/postmaster.pid" ]]; then
    PG_SOCKET_DIR="$(sed -n '5p' "$_PGRX_DATA/postmaster.pid" | tr -d '[:space:]')"
    PG_SOCKET_DIR="${PG_SOCKET_DIR:-/home/yonk/.pgrx}"
else
    PG_SOCKET_DIR="/home/yonk/.pgrx"
fi

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
OPT_MODELS=""
OPT_SCENARIOS=""
OPT_SCALE=1
OPT_TIMEOUT=180
OPT_FORCE=0

# ---------------------------------------------------------------------------
# Arg parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --models)    OPT_MODELS="$2";    shift 2 ;;
        --scenarios) OPT_SCENARIOS="$2"; shift 2 ;;
        --scale)     OPT_SCALE="$2";     shift 2 ;;
        --timeout)   OPT_TIMEOUT="$2";   shift 2 ;;
        --force)     OPT_FORCE=1;        shift   ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
log()  { echo "[bench] $*"; }
warn() { echo "[bench][WARN] $*" >&2; }

# Use the pgrx unix socket for all Postgres connections.
pg_run() {
    local db="$1"; shift
    PGPASSWORD="" PGHOST="$PG_SOCKET_DIR" psql -p "$PG_PORT" -U "$PG_USER" -d "$db" "$@" 2>&1
}

pg_exists_db() {
    local db="$1"
    pg_run postgres -Atc "SELECT 1 FROM pg_database WHERE datname='$db'" | grep -q 1
}

create_db() {
    local db="$1"
    log "Creating database $db"
    pg_run postgres -c "CREATE DATABASE \"$db\";" > /dev/null 2>&1 || true
}

drop_db() {
    local db="$1"
    pg_run postgres -c "DROP DATABASE IF EXISTS \"$db\";" > /dev/null 2>&1 || true
}

find_free_port() {
    python3 -c "import socket; s=socket.socket(); s.bind(('',0)); print(s.getsockname()[1]); s.close()"
}

wait_for_llama_server() {
    local port="$1" max_wait="${2:-90}"
    local elapsed=0
    log "Waiting for llama-cpp-server on port $port (max ${max_wait}s)..."
    while ! curl -sf "http://127.0.0.1:$port/v1/models" > /dev/null 2>&1; do
        sleep 2
        elapsed=$((elapsed + 2))
        if [[ $elapsed -ge $max_wait ]]; then
            return 1
        fi
    done
    log "llama-cpp-server ready on port $port (${elapsed}s)"
}

# Warm up a local llama-cpp-python server with one tiny chat completion.
# This ensures the model is fully loaded before timed scenario execution.
warmup_llama_server() {
    local port="$1" model_id="$2"
    log "Warming up llama-cpp-server on port $port ..."
    curl -sf -X POST "http://127.0.0.1:$port/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -d "{\"model\":\"${model_id}\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":1}" \
        > /dev/null 2>&1 || true
    log "Warmup done for port $port"
}

# ensure_pg_ready: verify Postgres is accepting connections and the extension
# is available. Retries psql up to ~20s; hard-fails if not available.
ensure_pg_ready() {
    local elapsed=0
    log "Ensuring Postgres is ready (socket=$PG_SOCKET_DIR port=$PG_PORT)..."
    while ! pg_run postgres -Atc "SELECT 1" > /dev/null 2>&1; do
        sleep 2
        elapsed=$((elapsed + 2))
        if [[ $elapsed -ge 20 ]]; then
            echo "[bench][FATAL] Postgres not reachable at socket=$PG_SOCKET_DIR port=$PG_PORT after ${elapsed}s. Aborting." >&2
            exit 1
        fi
    done
    log "Postgres reachable (${elapsed}s wait)."

    # Verify the extension is available (not necessarily installed, just available).
    local avail
    avail="$(pg_run postgres -Atc \
        "SELECT 1 FROM pg_available_extensions WHERE name='pg_synapse_pgrx'" 2>/dev/null || true)"
    if [[ "$avail" != "1" ]]; then
        echo "[bench][FATAL] pg_synapse_pgrx not found in pg_available_extensions." >&2
        echo "[bench][FATAL] Run: ./scripts/pgrx install ... --features pg17,... first (use the version-isolated wrapper, not bare cargo pgrx)." >&2
        exit 1
    fi
    log "pg_synapse_pgrx is available in pg_available_extensions."
}

# install_extension_with_retry: wraps CREATE EXTENSION with up to 3 attempts.
# On persistent failure, records the row with infra_error=true and returns 1.
# Usage: install_extension_with_retry <db> <mkey> <scenario> <scale> <kind> <run_date>
install_extension_with_retry() {
    local db="$1" mkey="$2" scenario="$3" scale="$4" kind="$5" run_date="$6"
    local attempt=0
    while [[ $attempt -lt 3 ]]; do
        if pg_run "$db" -c "CREATE EXTENSION IF NOT EXISTS pg_synapse_pgrx;" > /dev/null 2>&1; then
            return 0
        fi
        attempt=$((attempt + 1))
        if [[ $attempt -lt 3 ]]; then
            warn "  CREATE EXTENSION failed (attempt $attempt/3); retrying in 2s..."
            sleep 2
        fi
    done
    # All attempts failed: record infra-error row and signal caller to skip.
    local err_msg="failed to CREATE EXTENSION pg_synapse_pgrx after 3 attempts (infra)"
    warn "  $err_msg"
    record_result "$(python3 -c "import json; print(json.dumps({
        'model':'$mkey','scenario':'$scenario','scale':$scale,'kind':'$kind',
        'task_passed':False,'tool_emitted':False,
        'tokens_in':0,'tokens_out':0,'latency_ms':0,'iterations':0,
        'error':'$err_msg','infra_error':True,'run_date':'$run_date'
    }))")"
    return 1
}

# Parse a specific field from models.toml for a given key.
# Usage: toml_get <model_key> <field>
toml_get() {
    local mkey="$1" field="$2"
    # Extract the [[model]] block for key=mkey and get the field value.
    python3 - "$MODELS_TOML" "$mkey" "$field" <<'PYEOF'
import sys, re

path, mkey, field = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    text = f.read()

blocks = re.split(r'\[\[model\]\]', text)[1:]
for block in blocks:
    kv = {}
    for line in block.splitlines():
        m = re.match(r'^\s*(\w+)\s*=\s*"([^"]*)"', line)
        if m:
            kv[m.group(1)] = m.group(2)
    if kv.get('key') == mkey:
        print(kv.get(field, ''))
        sys.exit(0)
sys.exit(1)
PYEOF
}

# Return all model keys from models.toml.
all_model_keys() {
    python3 - "$MODELS_TOML" <<'PYEOF'
import sys, re
path = sys.argv[1]
with open(path) as f:
    text = f.read()
for block in re.split(r'\[\[model\]\]', text)[1:]:
    for line in block.splitlines():
        m = re.match(r'^\s*key\s*=\s*"([^"]*)"', line)
        if m:
            print(m.group(1))
            break
PYEOF
}

# Render a seed template: replace {{SCALE}} with the actual value.
render_seed() {
    local tmpl="$1" scale="$2"
    sed "s/{{SCALE}}/$scale/g" "$tmpl"
}

# Check if (model, scenario, scale) is already in results.jsonl.
already_done() {
    local mkey="$1" scenario="$2" scale="$3"
    if [[ ! -f "$RESULTS_JSONL" ]]; then return 1; fi
    python3 -c "
import json, sys
mkey, scenario, scale = sys.argv[1], sys.argv[2], int(sys.argv[3])
with open('$RESULTS_JSONL') as f:
    for line in f:
        row = json.loads(line)
        if row.get('model')==mkey and row.get('scenario')==scenario and row.get('scale')==scale:
            sys.exit(0)
sys.exit(1)
" "$mkey" "$scenario" "$scale" 2>/dev/null
}

# Append a result row to results.jsonl.
record_result() {
    echo "$1" >> "$RESULTS_JSONL"
}

# ---------------------------------------------------------------------------
# Resolve model list and scenario list
# ---------------------------------------------------------------------------
if [[ -n "$OPT_MODELS" ]]; then
    IFS=',' read -ra MODEL_KEYS <<< "$OPT_MODELS"
else
    mapfile -t MODEL_KEYS < <(all_model_keys)
fi

if [[ -n "$OPT_SCENARIOS" ]]; then
    IFS=',' read -ra SCENARIO_NAMES <<< "$OPT_SCENARIOS"
else
    mapfile -t SCENARIO_NAMES < <(ls "$SCENARIOS_DIR")
fi

log "Models:    ${MODEL_KEYS[*]}"
log "Scenarios: ${SCENARIO_NAMES[*]}"
log "Scale:     $OPT_SCALE"
log "Timeout:   ${OPT_TIMEOUT}s"

# ---------------------------------------------------------------------------
# Pre-flight: ensure Postgres is up and the extension is available.
# Fail fast here rather than failing every model cell.
# ---------------------------------------------------------------------------
ensure_pg_ready

# ---------------------------------------------------------------------------
# Global cleanup trap (handles local llama-cpp servers)
# ---------------------------------------------------------------------------
LLAMA_PIDS=()
cleanup() {
    for pid in "${LLAMA_PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------
RUN_DATE="$(date -u '+%Y-%m-%dT%H:%MZ')"

for MKEY in "${MODEL_KEYS[@]}"; do
    log "=== Model: $MKEY ==="

    KIND="$(toml_get "$MKEY" kind)" || { warn "Unknown model key: $MKEY"; continue; }

    LLAMA_PID=""
    LLAMA_PORT=""
    MODEL_PROFILE_BASE_URL=""
    MODEL_PROFILE_PROVIDER=""
    MODEL_PROFILE_MODEL_ID=""
    RESOLVED_API_KEY=""

    # ------------------------------------------------------------------
    # Setup: remote_openai
    # ------------------------------------------------------------------
    if [[ "$KIND" == "remote_openai" ]]; then
        MODEL_PROFILE_PROVIDER="openai"
        MODEL_PROFILE_BASE_URL="$(toml_get "$MKEY" base_url)"
        MODEL_PROFILE_MODEL_ID="$(toml_get "$MKEY" model)"
        API_KEY_ENV="$(toml_get "$MKEY" api_key_env 2>/dev/null || true)"

        if [[ -n "$API_KEY_ENV" && -f "$OPENAI_KEY_FILE" ]]; then
            # Extract key at runtime; never echo or log it.
            RESOLVED_API_KEY="$(sed -E 's/^api_key=//; s/[ \t\r\n]+$//' "$OPENAI_KEY_FILE")"
        fi

    # ------------------------------------------------------------------
    # Setup: local_gguf
    # ------------------------------------------------------------------
    elif [[ "$KIND" == "local_gguf" ]]; then
        MODEL_PROFILE_PROVIDER="llama-cpp"
        GGUF_REPO="$(toml_get "$MKEY" gguf_repo)"
        GGUF_FILE="$(toml_get "$MKEY" gguf_file)"
        MODEL_PROFILE_MODEL_ID="$(toml_get "$MKEY" served_model_id)"

        GGUF_PATH="$MODEL_CACHE_DIR/$MKEY/model.gguf"
        if [[ ! -f "$GGUF_PATH" ]]; then
            mkdir -p "$(dirname "$GGUF_PATH")"
            GGUF_URL="https://huggingface.co/$GGUF_REPO/resolve/main/$GGUF_FILE"
            log "Downloading $GGUF_URL -> $GGUF_PATH"
            curl -L --retry 3 --progress-bar -o "$GGUF_PATH" "$GGUF_URL" || {
                warn "Failed to download $MKEY; skipping."
                continue
            }
        else
            log "GGUF already cached: $GGUF_PATH"
        fi

        LLAMA_PORT="$(find_free_port)"
        MODEL_PROFILE_BASE_URL="http://127.0.0.1:$LLAMA_PORT/v1"

        # Optional per-model server flags from models.toml.
        CHAT_FORMAT="$(toml_get "$MKEY" chat_format 2>/dev/null || true)"
        EXTRA_SERVER_ARGS="$(toml_get "$MKEY" server_args 2>/dev/null || true)"

        log "Starting llama-cpp-server on port $LLAMA_PORT for $MKEY"
        LAUNCH_CMD=("$LLAMA_SERVER_BIN" -m llama_cpp.server
            --model "$GGUF_PATH"
            --port "$LLAMA_PORT"
            --n_ctx 4096)
        if [[ -n "$CHAT_FORMAT" ]]; then
            LAUNCH_CMD+=(--chat_format "$CHAT_FORMAT")
            log "  chat_format: $CHAT_FORMAT"
        fi
        if [[ -n "$EXTRA_SERVER_ARGS" ]]; then
            # Word-split intentional: server_args is a space-separated flags string.
            # shellcheck disable=SC2086
            LAUNCH_CMD+=($EXTRA_SERVER_ARGS)
            log "  extra server_args: $EXTRA_SERVER_ARGS"
        fi
        "${LAUNCH_CMD[@]}" > "/tmp/llama_${MKEY}.log" 2>&1 &
        LLAMA_PID=$!
        LLAMA_PIDS+=("$LLAMA_PID")

        if ! wait_for_llama_server "$LLAMA_PORT" 90; then
            warn "llama-cpp-server for $MKEY did not start; skipping."
            kill "$LLAMA_PID" 2>/dev/null || true
            continue
        fi

        # Warm up: one tiny completion to force model load before timed scenarios.
        warmup_llama_server "$LLAMA_PORT" "$MODEL_PROFILE_MODEL_ID"
    else
        warn "Unknown kind '$KIND' for $MKEY; skipping."
        continue
    fi

    # ------------------------------------------------------------------
    # Per-scenario loop
    # ------------------------------------------------------------------
    for SCENARIO in "${SCENARIO_NAMES[@]}"; do
        SCENARIO_DIR="$SCENARIOS_DIR/$SCENARIO"
        if [[ ! -d "$SCENARIO_DIR" ]]; then
            warn "Scenario directory not found: $SCENARIO_DIR; skipping."
            continue
        fi

        log "  Scenario: $SCENARIO"

        # Load per-scenario metadata. Defaults: KIND=sql,
        # TOOLS=sql_query,sql_exec, MAX_ITER=25. The 25 default matches
        # LangGraph's recursion_limit; simple scenarios use far fewer,
        # multi-step agents (a1_ingest, a3_triage) declare a higher
        # MAX_ITER in their meta.env. pg_synapse honors the per-agent
        # max_iterations column; this passes a scenario-appropriate value
        # instead of a hardcoded constant.
        SCENARIO_KIND="sql"
        SCENARIO_TOOLS="sql_query,sql_exec"
        SCENARIO_MAX_ITER="25"
        if [[ -f "$SCENARIO_DIR/meta.env" ]]; then
            # shellcheck source=/dev/null
            source "$SCENARIO_DIR/meta.env"
            SCENARIO_KIND="${KIND:-sql}"
            SCENARIO_TOOLS="${TOOLS:-sql_query,sql_exec}"
            SCENARIO_MAX_ITER="${MAX_ITER:-25}"
        fi
        log "  Kind: $SCENARIO_KIND  Tools: $SCENARIO_TOOLS  MaxIter: $SCENARIO_MAX_ITER"

        if [[ $OPT_FORCE -eq 0 ]] && already_done "$MKEY" "$SCENARIO" "$OPT_SCALE"; then
            log "  Already done ($MKEY/$SCENARIO/scale=$OPT_SCALE); skipping. Use --force to re-run."
            continue
        fi

        DB="bench_${MKEY}_${SCENARIO}"
        # Sanitize: replace dots and special chars with underscores.
        DB="$(echo "$DB" | tr '.-' '__')"
        AGENT_NAME="bench_agent"
        PROFILE_NAME="bench_profile"

        # Initialize result fields.
        TASK_PASSED="false"
        TOOL_EMITTED="false"
        TOKENS_IN=0
        TOKENS_OUT=0
        LATENCY_MS=0
        ITERATIONS=0
        ERROR=""
        EXEC_JSON=""

        # For fs scenarios, compute the per-run sandbox dir and FSDIR token.
        # FSDIR is the relative path from the pgrx fs sandbox root (/tmp/pg_synapse_fs).
        # DB is already "bench_<sanitized_model>_<scenario>", so use it directly.
        FS_SANDBOX_ROOT="/tmp/pg_synapse_fs"
        FSDIR_RELPATH="$DB"
        FS_RUN_DIR="${FS_SANDBOX_ROOT}/${FSDIR_RELPATH}"

        # Cleanup any prior DB.
        drop_db "$DB"
        create_db "$DB"

        # Install extension (with retry; records infra_error row on persistent failure).
        if ! install_extension_with_retry \
                "$DB" "$MKEY" "$SCENARIO" "$OPT_SCALE" "$SCENARIO_KIND" "$RUN_DATE"; then
            drop_db "$DB"
            continue
        fi

        if [[ "$SCENARIO_KIND" == "sql" ]]; then
            # Apply SQL seed (render template first).
            SEED_TMPL="$SCENARIO_DIR/seed.sql.tmpl"
            if [[ -f "$SEED_TMPL" ]]; then
                RENDERED_SEED="$(mktemp /tmp/bench_seed_XXXXXX.sql)"
                render_seed "$SEED_TMPL" "$OPT_SCALE" > "$RENDERED_SEED"
                if ! pg_run "$DB" -f "$RENDERED_SEED" > /dev/null 2>&1; then
                    ERROR="seed.sql failed"
                    warn "  $ERROR for $MKEY/$SCENARIO"
                    rm -f "$RENDERED_SEED"
                    drop_db "$DB"
                    record_result "$(python3 -c "import json; print(json.dumps({'model':'$MKEY','scenario':'$SCENARIO','scale':$OPT_SCALE,'kind':'$SCENARIO_KIND','task_passed':False,'tool_emitted':False,'tokens_in':0,'tokens_out':0,'latency_ms':0,'iterations':0,'error':'$ERROR','run_date':'$RUN_DATE'}))")"
                    continue
                fi
                rm -f "$RENDERED_SEED"
            fi
            # Optional fs seeding for cross-toolset sql scenarios.
            # If seed_fs.sh exists alongside the sql seed, run it so the agent
            # can read files from the sandbox in addition to querying Postgres.
            SEED_FS_SQL="$SCENARIO_DIR/seed_fs.sh"
            if [[ -f "$SEED_FS_SQL" ]]; then
                rm -rf "$FS_RUN_DIR"
                mkdir -p "$FS_RUN_DIR"
                if ! FS_ROOT="$FS_RUN_DIR" SCALE="$OPT_SCALE" bash "$SEED_FS_SQL"; then
                    ERROR="seed_fs.sh failed"
                    warn "  $ERROR for $MKEY/$SCENARIO"
                    drop_db "$DB"
                    record_result "$(python3 -c "import json; print(json.dumps({'model':'$MKEY','scenario':'$SCENARIO','scale':$OPT_SCALE,'kind':'$SCENARIO_KIND','task_passed':False,'tool_emitted':False,'tokens_in':0,'tokens_out':0,'latency_ms':0,'iterations':0,'error':'$ERROR','run_date':'$RUN_DATE'}))")"
                    continue
                fi
                log "  FS seed (sql+fs) done: $FS_RUN_DIR"
            fi
        else
            # KIND=fs: prepare a fresh per-run sandbox dir and run seed_fs.sh.
            rm -rf "$FS_RUN_DIR"
            mkdir -p "$FS_RUN_DIR"
            SEED_FS="$SCENARIO_DIR/seed_fs.sh"
            if [[ -f "$SEED_FS" ]]; then
                if ! FS_ROOT="$FS_RUN_DIR" SCALE="$OPT_SCALE" bash "$SEED_FS"; then
                    ERROR="seed_fs.sh failed"
                    warn "  $ERROR for $MKEY/$SCENARIO"
                    drop_db "$DB"
                    record_result "$(python3 -c "import json; print(json.dumps({'model':'$MKEY','scenario':'$SCENARIO','scale':$OPT_SCALE,'kind':'$SCENARIO_KIND','task_passed':False,'tool_emitted':False,'tokens_in':0,'tokens_out':0,'latency_ms':0,'iterations':0,'error':'$ERROR','run_date':'$RUN_DATE'}))")"
                    continue
                fi
                log "  FS seed done: $FS_RUN_DIR"
            fi
        fi

        # Register LLM profile.
        # Key is injected via literal in the params JSONB; never echoed to stdout.
        if [[ -n "$RESOLVED_API_KEY" ]]; then
            # Use a temp SQL file to avoid the key appearing in the process list.
            KEY_SQL="$(mktemp /tmp/bench_key_XXXXXX.sql)"
            # Write key SQL to temp file; it is never echoed.
            python3 - "$KEY_SQL" "$PROFILE_NAME" "$MODEL_PROFILE_PROVIDER" \
                "$MODEL_PROFILE_MODEL_ID" "$MODEL_PROFILE_BASE_URL" "$RESOLVED_API_KEY" <<'PYEOF'
import sys, json
out, name, provider, model, base_url, key = sys.argv[1:]
params = json.dumps({"_resolved_api_key": key})
sql = (
    "SELECT synapse.llm_profile_set("
    + repr(name) + ", "
    + repr(provider) + ", "
    + repr(model) + ", "
    + repr(base_url) + ", "
    + "NULL, "
    + "'" + params.replace("'", "''") + "'::jsonb"
    + ");"
)
with open(out, 'w') as f:
    f.write(sql)
PYEOF
            pg_run "$DB" -f "$KEY_SQL" > /dev/null 2>&1
            rm -f "$KEY_SQL"
        else
            pg_run "$DB" -Atc "SELECT synapse.llm_profile_set(
                '$PROFILE_NAME',
                '$MODEL_PROFILE_PROVIDER',
                '$MODEL_PROFILE_MODEL_ID',
                '$MODEL_PROFILE_BASE_URL',
                NULL,
                '{}'::jsonb
            );" > /dev/null 2>&1
        fi

        # Read agent config files.
        SYSTEM_PROMPT="$(cat "$SCENARIO_DIR/system_prompt.txt")"
        TASK="$(cat "$SCENARIO_DIR/task.txt")"

        # Substitute {{FSDIR}} for fs scenarios and for sql scenarios that also
        # have a seed_fs.sh (cross-toolset: Postgres tables plus file fixtures).
        if [[ "$SCENARIO_KIND" == "fs" ]] || \
           [[ "$SCENARIO_KIND" == "sql" && -f "$SCENARIO_DIR/seed_fs.sh" ]]; then
            SYSTEM_PROMPT="${SYSTEM_PROMPT//\{\{FSDIR\}\}/$FSDIR_RELPATH}"
            TASK="${TASK//\{\{FSDIR\}\}/$FSDIR_RELPATH}"
        fi

        # Also substitute {{SCALE}} in task/system_prompt (same as seed rendering).
        SYSTEM_PROMPT="${SYSTEM_PROMPT//\{\{SCALE\}\}/$OPT_SCALE}"
        TASK="${TASK//\{\{SCALE\}\}/$OPT_SCALE}"

        # Escape single quotes for SQL.
        SYSTEM_PROMPT_ESC="${SYSTEM_PROMPT//\'/\'\'}"
        TASK_ESC="${TASK//\'/\'\'}"

        # Build ARRAY literal from SCENARIO_TOOLS (comma-separated tool names).
        TOOLS_ARRAY_SQL="$(python3 -c "
import sys
tools = sys.argv[1].split(',')
arr = 'ARRAY[' + ','.join(repr(t.strip()) for t in tools) + ']'
print(arr)
" "$SCENARIO_TOOLS")"

        # Register agent.
        pg_run "$DB" -Atc "SELECT synapse.agent_create(
            '$AGENT_NAME',
            \$\$${SYSTEM_PROMPT_ESC}\$\$,
            'conversation',
            '$PROFILE_NAME',
            ${TOOLS_ARRAY_SQL},
            ${SCENARIO_MAX_ITER},
            $((OPT_TIMEOUT * 1000))
        );" > /dev/null 2>&1 || {
            ERROR="agent_create failed"
            warn "  $ERROR"
            drop_db "$DB"
            record_result "$(python3 -c "import json; print(json.dumps({'model':'$MKEY','scenario':'$SCENARIO','scale':$OPT_SCALE,'kind':'$SCENARIO_KIND','task_passed':False,'tool_emitted':False,'tokens_in':0,'tokens_out':0,'latency_ms':0,'iterations':0,'error':'$ERROR','run_date':'$RUN_DATE'}))")"
            continue
        }

        # Run agent execute() with statement_timeout.
        # Use a separate -c for SET so psql -Atc only outputs the JSON row.
        # On a network/transport error (local model server hiccup), retry once.
        TIMEOUT_MS="$((OPT_TIMEOUT * 1000))"
        pg_run "$DB" -c "SET statement_timeout = ${TIMEOUT_MS};" > /dev/null 2>&1 || true
        EXEC_JSON="$(pg_run "$DB" -Atc \
            "SELECT synapse.execute('$AGENT_NAME', \$\$${TASK_ESC}\$\$);" 2>&1)" || true

        # If the raw output looks like a network error and no JSON row was found,
        # retry the execute once (handles transient local server hiccups).
        if echo "$EXEC_JSON" | grep -qi "network error\|error sending request"; then
            warn "  Network error detected; retrying execute once..."
            pg_run "$DB" -c "SET statement_timeout = ${TIMEOUT_MS};" > /dev/null 2>&1 || true
            EXEC_JSON="$(pg_run "$DB" -Atc \
                "SELECT synapse.execute('$AGENT_NAME', \$\$${TASK_ESC}\$\$);" 2>&1)" || true
        fi

        # Parse execution JSON. The raw EXEC_JSON may contain error lines from
        # psql before the JSON row; extract the first JSON object found.
        EXEC_JSON_PARSED="$(echo "$EXEC_JSON" | python3 -c "
import sys, json
for line in sys.stdin:
    line = line.strip()
    if line.startswith('{'):
        try:
            print(json.dumps(json.loads(line)))
            break
        except Exception:
            pass
" 2>/dev/null || echo '{}')"

        TOKENS_IN="$(echo "$EXEC_JSON_PARSED" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('tokens_in',0))" 2>/dev/null || echo 0)"
        TOKENS_OUT="$(echo "$EXEC_JSON_PARSED" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('tokens_out',0))" 2>/dev/null || echo 0)"
        LATENCY_MS="$(echo "$EXEC_JSON_PARSED" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('duration_ms',0))" 2>/dev/null || echo 0)"
        ITERATIONS="$(echo "$EXEC_JSON_PARSED" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('iterations',0))" 2>/dev/null || echo 0)"

        # Check if tool calls were emitted. Prefer the ground truth in
        # synapse.messages (survives mid-loop errors), fall back to the
        # executor JSON for scenarios where the DB was dropped early.
        TOOL_EMITTED_DB="$(pg_run "$DB" -Atc \
            "SELECT EXISTS(SELECT 1 FROM synapse.messages WHERE role='tool' LIMIT 1);" 2>/dev/null || echo '')"
        if [[ "$TOOL_EMITTED_DB" == "t" ]]; then
            TOOL_EMITTED="true"
        elif [[ "$TOOL_EMITTED_DB" == "f" ]]; then
            TOOL_EMITTED="false"
        else
            TOOL_EMITTED="$(echo "$EXEC_JSON_PARSED" | python3 -c "
import json, sys
d = json.loads(sys.stdin.read())
tcs = d.get('tool_calls', [])
print('true' if tcs else 'false')
" 2>/dev/null || echo 'false')"
        fi

        # Capture any error from execution.
        EXEC_ERROR="$(echo "$EXEC_JSON_PARSED" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('error',''))" 2>/dev/null || true)"
        if [[ -n "$EXEC_ERROR" ]]; then
            ERROR="$EXEC_ERROR"
            warn "  Agent error: $ERROR"
        fi

        # Run assertion (SQL for KIND=sql, shell script for KIND=fs).
        if [[ "$SCENARIO_KIND" == "sql" ]]; then
            ASSERT_SQL="$SCENARIO_DIR/assert.sql"
            if [[ -f "$ASSERT_SQL" ]]; then
                ASSERT_RESULT="$(pg_run "$DB" -Atc "$(cat "$ASSERT_SQL")" 2>/dev/null || echo 'false')"
                if [[ "$ASSERT_RESULT" == "t" || "$ASSERT_RESULT" == "true" ]]; then
                    TASK_PASSED="true"
                else
                    TASK_PASSED="false"
                fi
            fi
        else
            ASSERT_FS="$SCENARIO_DIR/assert_fs.sh"
            if [[ -f "$ASSERT_FS" ]]; then
                if FS_ROOT="$FS_RUN_DIR" bash "$ASSERT_FS" > /dev/null 2>&1; then
                    TASK_PASSED="true"
                else
                    TASK_PASSED="false"
                    ASSERT_DETAIL="$(FS_ROOT="$FS_RUN_DIR" bash "$ASSERT_FS" 2>&1 || true)"
                    log "  FS assert detail: $ASSERT_DETAIL"
                fi
            fi
        fi

        log "  Result: passed=$TASK_PASSED tool=$TOOL_EMITTED tokens_in=$TOKENS_IN tokens_out=$TOKENS_OUT latency=${LATENCY_MS}ms"

        # Classify infra errors so consumers can exclude them from model verdicts.
        INFRA_ERROR="false"
        if echo "${ERROR:-}" | grep -qi "network error\|error sending request\|CREATE EXTENSION"; then
            INFRA_ERROR="true"
        fi

        # Record JSON line. Use python3 json.dumps for all values to avoid
        # bash interpolation breaking JSON booleans or special chars in error.
        record_result "$(python3 - \
            "$MKEY" "$SCENARIO" "$OPT_SCALE" "$SCENARIO_KIND" \
            "$TASK_PASSED" "$TOOL_EMITTED" \
            "${TOKENS_IN:-0}" "${TOKENS_OUT:-0}" \
            "${LATENCY_MS:-0}" "${ITERATIONS:-0}" \
            "${ERROR:-}" "$INFRA_ERROR" "$RUN_DATE" <<'PYEOF'
import json, sys
(_, mkey, scenario, scale, kind, task_passed, tool_emitted,
 tokens_in, tokens_out, latency_ms, iterations, error, infra_error, run_date) = sys.argv
row = {
    'model': mkey,
    'scenario': scenario,
    'scale': int(scale),
    'kind': kind,
    'task_passed': task_passed == 'true',
    'tool_emitted': tool_emitted == 'true',
    'tokens_in': int(tokens_in) if tokens_in.isdigit() else 0,
    'tokens_out': int(tokens_out) if tokens_out.isdigit() else 0,
    'latency_ms': int(latency_ms) if latency_ms.isdigit() else 0,
    'iterations': int(iterations) if iterations.isdigit() else 0,
    'error': error[:300],
    'infra_error': infra_error == 'true',
    'run_date': run_date,
}
print(json.dumps(row))
PYEOF
)"

        # Tear down DB.
        drop_db "$DB"

    done  # scenarios

    # ------------------------------------------------------------------
    # Teardown: local server
    # ------------------------------------------------------------------
    if [[ -n "$LLAMA_PID" ]]; then
        log "Stopping llama-cpp-server (pid $LLAMA_PID) for $MKEY"
        kill "$LLAMA_PID" 2>/dev/null || true
        LLAMA_PIDS=("${LLAMA_PIDS[@]/$LLAMA_PID}")
        LLAMA_PID=""
    fi

done  # models

# ---------------------------------------------------------------------------
# Regenerate RESULTS.md leaderboard
# ---------------------------------------------------------------------------
log "Generating $RESULTS_MD ..."

python3 - "$RESULTS_JSONL" "$RESULTS_MD" "$OPT_SCALE" "$RUN_DATE" <<'PYEOF'
import json, sys, os
from collections import defaultdict

results_file, out_file, scale, run_date = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

rows = []
if os.path.exists(results_file):
    with open(results_file) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))

# Group by (model, scenario, scale): keep last entry (most recent run).
latest = {}
for r in rows:
    k = (r['model'], r['scenario'], r['scale'])
    latest[k] = r

# Collect unique model keys and scenario names.
models = list(dict.fromkeys(r['model'] for r in latest.values()))
scenarios = list(dict.fromkeys(r['scenario'] for r in latest.values()))

def cell(r):
    if r is None:
        return '-'
    tp = r.get('task_passed', False)
    te = r.get('tool_emitted', False)
    tin = r.get('tokens_in', 0)
    tout = r.get('tokens_out', 0)
    err = r.get('error', '')
    if tp:
        label = 'PASS'
    elif te:
        label = 'TOOL?'
    else:
        label = 'FAIL'
    if err:
        label += '(err)'
    return f'{label} {tin}in/{tout}out'

lines = []
lines.append(f'# pg_synapse model benchmark leaderboard')
lines.append(f'')
lines.append(f'Run date: {run_date}  Scale: {scale}')
lines.append(f'')
lines.append('Cells: PASS/FAIL/TOOL? + tokens_in/tokens_out. TOOL? = model emitted tool calls but task assertion failed.')
lines.append('')

# Table header.
header = '| model | ' + ' | '.join(scenarios) + ' | passed |'
sep    = '|-------|' + '|'.join(['-' * (len(s) + 2) for s in scenarios]) + '|--------|'
lines.append(header)
lines.append(sep)

ranking = []
for m in models:
    passed = 0
    cells = []
    for s in scenarios:
        r = latest.get((m, s, int(scale)))
        cells.append(cell(r))
        if r and r.get('task_passed'):
            passed += 1
    row = f'| {m} | ' + ' | '.join(cells) + f' | {passed}/{len(scenarios)} |'
    lines.append(row)
    total_tokens = sum(
        (latest.get((m, s, int(scale)), {}).get('tokens_in', 0) or 0) +
        (latest.get((m, s, int(scale)), {}).get('tokens_out', 0) or 0)
        for s in scenarios
    )
    ranking.append((m, passed, total_tokens))

lines.append('')

# Summary ranking (by scenarios passed desc, then total tokens asc).
ranking.sort(key=lambda x: (-x[1], x[2]))
lines.append('## Summary ranking')
lines.append('')
lines.append('| rank | model | scenarios passed | total tokens |')
lines.append('|------|-------|-----------------|--------------|')
for i, (m, passed, tokens) in enumerate(ranking, 1):
    lines.append(f'| {i} | {m} | {passed}/{len(scenarios)} | {tokens} |')

lines.append('')
lines.append('## Notes')
lines.append('')
notes = []
for m in models:
    never_tool = all(
        not (latest.get((m, s, int(scale)), {}).get('tool_emitted', False))
        for s in scenarios
        if (m, s, int(scale)) in latest
    )
    if never_tool and any((m, s, int(scale)) in latest for s in scenarios):
        notes.append(f'- {m}: never emitted tool calls across all tested scenarios.')
for n in notes:
    lines.append(n)
if not notes:
    lines.append('No model-level concerns to report.')

lines.append('')
with open(out_file, 'w') as f:
    f.write('\n'.join(lines) + '\n')

print(f'Wrote {out_file}')
PYEOF

log "Done. Results: $RESULTS_JSONL"
log "Leaderboard: $RESULTS_MD"
