# pg_synapse real-world agent specs

Three end-to-end agents that exercise pg_synapse as a flexible tool-using
runtime. Each is a benchmark scenario (driven by `bench/run_bench.sh`) with a
deterministic post-condition, so it doubles as a regression test and a model
capability probe. All are built clean-room from public sources; Agent 3 is an
expansion of our existing `examples/customer-support-triage/`, NOT a copy of
any private codebase.

Target models (the ones that score WORKS on neutral fs tools): remote
(vllm-qwen3-coder, openai-gpt5-mini) and local (gemma-4-E2B-it,
gemma-4-E4B-it, qwen3-4b-2507, qwen3.5-9b).

---

## Agent 1: Data Ingest (`a1_ingest`)

**Real-world job:** an operator drops a CSV and a JSON file into a directory;
an agent loads both into a normalized Postgres table without hand-written ETL.

**Toolset:** `read_file` (fs) + `sql_exec`, `sql_query` (sql). Cross-toolset.

**Fixtures (seeded):**
- `{{FSDIR}}/incoming/customers.csv`: header `id,name,email,country`, 5 rows.
- `{{FSDIR}}/incoming/orders.json`: array of 6 objects
  `{order_id, customer_id, amount, status}`.
- DB: empty tables `ingest.customers(id int pk, name text, email text,
  country text)` and `ingest.orders(order_id int pk, customer_id int,
  amount numeric, status text)` created by the SQL seed.

**Step flow the agent must perform:**
1. `read_file incoming/customers.csv`, parse the CSV.
2. For each row, `sql_exec` an INSERT into `ingest.customers` (parameterized).
3. `read_file incoming/orders.json`, parse the JSON array.
4. For each object, `sql_exec` an INSERT into `ingest.orders`.
5. `sql_query` a count of both tables, report the totals.

**Deterministic success:** `ingest.customers` has exactly 5 rows matching the
CSV, `ingest.orders` has exactly 6 rows matching the JSON, foreign-key-ish
consistency holds (every order.customer_id exists in customers). Assertion is
a SQL boolean comparing loaded data against the known fixture.

**Scenario dir:** `bench/scenarios/a1_ingest/` (KIND=fs+sql; meta.env
TOOLS=read_file,sql_exec,sql_query).

---

## Agent 2: Distill + Sentiment (`a2_distill`)

**Real-world job:** a table holds free-text customer feedback; an agent
condenses it to a short digest, tags each item with sentiment, and runs the
result through a context-compression tool (lede) to produce a token-bounded
brief suitable for an executive summary.

**Toolset:** `sql_query`, `sql_exec` (sql) + `lede_compress` (new
`pg-synapse-tools-lede` plugin). The lede tool is a documented SHIM: it shells
to a `lede` CLI if one is on PATH, otherwise it performs deterministic
extractive compression (rank sentences by salience, keep top-k under a token
budget). Real lede integration is a v0.2 plugin per the design spec
non-goals; the shim proves the agent pattern.

**Fixtures (seeded):**
- DB table `feedback.items(id int pk, body text)` with 8 deterministic rows
  of mixed-sentiment customer comments.
- Empty `feedback.digest(id int pk, sentiment text, gist text)`.

**Step flow:**
1. `sql_query SELECT id, body FROM feedback.items`.
2. For each item: classify sentiment in {positive, negative, neutral},
   write a one-line gist, `sql_exec` an INSERT into `feedback.digest`.
3. Call `lede_compress` with the concatenated gists and a token budget to get
   a final executive brief.
4. Report the brief.

**Deterministic success:** `feedback.digest` has exactly 8 rows, every
`sentiment` is one of the three allowed labels, and `lede_compress` returned
a non-empty brief shorter than the input (token-bounded). The sentiment
correctness is graded loosely (>= 6/8 match a known-good labeling) so the
test measures the agent loop, not sentiment perfection.

**Scenario dir:** `bench/scenarios/a2_distill/` (KIND=sql; meta.env
TOOLS=sql_query,sql_exec,lede_compress).

---

## Agent 3: Support Triage (`a3_triage`)

**Real-world job:** inbound support tickets arrive unclassified; an agent
reads each open ticket, looks up the customer tier, assigns category and
priority, decides escalation (enterprise + urgent escalates), updates the
ticket, and appends an audit row. Expansion of the existing clean-room
`examples/customer-support-triage/` into a fuller workflow with an audit
trail.

**Toolset:** `sql_query`, `sql_exec` (sql). Multi-step reasoning + writes.

**Fixtures (seeded):**
- `support.customers(id, email, tier)` with free/pro/enterprise rows.
- `support.tickets(id, customer_id, subject, body, category, priority,
  escalated, handled_at)` with {{SCALE}}*4 open tickets (category NULL),
  deterministic content spanning api/billing/howto/other concerns and all
  tiers.
- Empty `support.audit(ticket_id int, action text, at timestamptz default
  now())`.

**Step flow (per open ticket):**
1. `sql_query` the ticket joined to its customer tier.
2. Decide category in {api, billing, howto, other}, priority in
   {low, normal, high, urgent}. Enterprise-tier + urgent => escalated=true.
3. `sql_exec` UPDATE the ticket (category, priority, escalated, handled_at).
4. `sql_exec` INSERT a row into `support.audit` describing the action.
5. After all tickets: `sql_query` a summary count by category, report it.

**Deterministic success:** every ticket has non-null category, priority in
the allowed set, escalated set correctly for every enterprise+urgent ticket
(checked against the known fixture logic), `support.audit` has one row per
ticket. SQL boolean assertion.

**Scenario dir:** `bench/scenarios/a3_triage/` (KIND=sql; meta.env
TOOLS=sql_query,sql_exec). Scales by {{SCALE}}.

---

## Build + bench

Each agent ships as a `bench/scenarios/<name>/` scenario (meta.env,
seed files, task.txt, system_prompt.txt, assertion) so the existing harness
runs it across the model matrix. Agent 2 also ships the
`pg-synapse-tools-lede` plugin wired into the pgrx + sidecar hosts. After all
three land, run them across the WORKS-tier models and fold the results into
`docs/MODEL-COMPATIBILITY.md`.
