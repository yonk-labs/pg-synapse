#!/usr/bin/env bash
# NL2SQL-with-pg-lexicon demo driver. Assumes the four services in README.md
# are already running (test DB :5439, pg-lexicon serve :9777, mock LLM :8991,
# sidecar :8089). Registers the profile+agent (idempotent), runs one NL
# question through the full loop, and asserts the JOIN result is correct.
set -euo pipefail

SIDE="http://127.0.0.1:8089"
H="X-PG-Synapse-Admin-Token: demo-admin-token"
TRACE=/tmp/nl2sql-trace.log

echo "1. preflight"
curl -sf "$SIDE/v1/health" >/dev/null || { echo "sidecar :8089 down, see README"; exit 1; }
curl -sf http://127.0.0.1:9777/healthz >/dev/null || { echo "pg-lexicon :9777 down, see README"; exit 1; }

echo "2. register LLM profile (mock) + schema-free agent"
curl -sf -X POST "$SIDE/v1/admin/profile/llm" -H "$H" -H "Content-Type: application/json" -d '{
  "name":"mock","provider":"openai","model":"mock-nl2sql",
  "base_url":"http://127.0.0.1:8991/v1","params":{"api_key":"sk-mock"}}' >/dev/null
curl -sf -X POST "$SIDE/v1/admin/agent" -H "$H" -H "Content-Type: application/json" -d '{
  "name":"shop_analyst",
  "system_prompt":"You are a SQL analyst for a shop database. You do NOT know the schema in advance. For any question, FIRST call get_schema_context with the question to retrieve the relevant tables, columns, and foreign-key relationships. THEN write a single SQL query using that context and call sql_query to run it. Finally, report the result.",
  "executor_name":"conversation","llm_profile_main":"mock",
  "tools":["get_schema_context","sql_query"]}' >/dev/null

echo "3. execute NL question through the full loop"
: > "$TRACE"
curl -sf -X POST "$SIDE/v1/execute" -H "Content-Type: application/json" \
  -d '{"agent":"shop_analyst","input":"What is the total revenue per customer?"}' \
  | python3 -c 'import json,sys;print("   final:",json.load(sys.stdin)["output"])'

echo "4. assert the JOIN result the agent computed is correct"
ROWS=$(grep -o 'call_sql.*' "$TRACE" | sed 's/^[^[]*//')
echo "   rows: $ROWS"
python3 - "$ROWS" <<'PY'
import json, sys
rows = json.loads(sys.argv[1])
got = {r["customer"]: r["revenue"] for r in rows}
want = {"Bob Martinez": 349.99, "Alice Nguyen": 221.96, "Carol Smith": 65.98, "David Lee": 25.99}
assert got == want, f"MISMATCH\n got={got}\n want={want}"
assert "Eve Johnson" not in got, "Eve has no orders and must not appear"
print("   PASS: revenue-per-customer JOIN correct, schema came only from pg-lexicon")
PY
