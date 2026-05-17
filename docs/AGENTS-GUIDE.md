# pg_synapse Agents Guide

An agent in pg_synapse is a row in `synapse.agents`. You create one with
`synapse.agent_create`, then invoke it from any Postgres client:

```sql
SELECT synapse.execute('my_agent', 'what do you need done?');
```

The agent runs a real tool-calling loop against a language model. Tools are
loaded from plugin crates and are named in the agent row's `tools` array.
A profile row (`synapse.llm_profiles`) binds a provider, model, and endpoint
to the agent.

All six agents described below were validated against an OpenAI-compatible
vLLM endpoint. A live model scorecard lives in `docs/MODEL-COMPATIBILITY.md`.

---

## Agent 1: Data Ingest (a1_ingest)

**What it does.** Reads a CSV file and a JSON file from a sandbox directory
and loads both into normalized Postgres tables. This is a no-ETL-code ingest
pattern: drop files, invoke agent, rows appear.

**Tools used.** `read_file`, `sql_query`, `sql_exec`

**Schema.** Seed creates an empty `ingest` schema with two target tables:

```sql
CREATE SCHEMA IF NOT EXISTS ingest;

CREATE TABLE IF NOT EXISTS ingest.customers (
    id      INT PRIMARY KEY,
    name    TEXT NOT NULL,
    email   TEXT NOT NULL,
    country TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ingest.orders (
    order_id    INT PRIMARY KEY,
    customer_id INT NOT NULL,
    amount      NUMERIC NOT NULL,
    status      TEXT NOT NULL
);
```

Fixture files written to the sandbox before the agent runs:

```
incoming/customers.csv   -- header row, then 5 data rows
  id,name,email,country
  1,Alice Nguyen,alice@example.com,US
  2,Bob Patel,bob@example.com,IN
  3,Carmen Lopez,carmen@example.com,MX
  4,David Kim,david@example.com,KR
  5,Eva Rossi,eva@example.com,IT

incoming/orders.json     -- JSON array of 6 objects
  [
    {"order_id": 101, "customer_id": 1, "amount": 49.99, "status": "completed"},
    {"order_id": 102, "customer_id": 2, "amount": 129.00, "status": "pending"},
    {"order_id": 103, "customer_id": 1, "amount": 19.50, "status": "completed"},
    {"order_id": 104, "customer_id": 3, "amount": 75.25, "status": "shipped"},
    {"order_id": 105, "customer_id": 4, "amount": 200.00, "status": "completed"},
    {"order_id": 106, "customer_id": 5, "amount": 33.10, "status": "pending"}
  ]
```

**Sample code.**

```sql
-- Point at an OpenAI-compatible endpoint.
SELECT synapse.llm_profile_set(
  'ingest-profile',
  'openai',
  'your-model-name-here',
  'http://your-llm-endpoint:8000/v1',
  NULL,
  '{}'::jsonb
);

-- Create the agent.
SELECT synapse.agent_create(
  'a1_ingest',
  $$You are a data ingest agent. You have three tools: read_file, sql_exec, and sql_query.

Target tables (already created, currently empty):

  ingest.customers(id INT, name TEXT, email TEXT, country TEXT)
  ingest.orders(order_id INT, customer_id INT, amount NUMERIC, status TEXT)

Rules:
- Use read_file to read a file before inserting its contents.
- File paths are relative to the sandbox root (no leading slash).
- The customers CSV has a header row on line 1. Skip it; parse the remaining lines.
- Insert ALL rows of a file in a SINGLE sql_exec call using one multi-row INSERT.
  Do not issue one INSERT per row. Use parameterized $1, $2, ... placeholders.
- The orders file is a JSON array. Each element has keys: order_id, customer_id,
  amount, status. Insert all of them with one multi-row INSERT.
- After loading both files, use sql_query to count rows in each table and report totals.
- Do not skip rows. Do not guess data; only insert what you read from the files.
- Call the tools; do not just describe what you would do.$$,
  'conversation',
  'ingest-profile',
  ARRAY['read_file', 'sql_query', 'sql_exec'],
  40,
  120000
);

-- Run the task (FSDIR is set by the harness; in manual runs, use the real path).
SELECT synapse.execute(
  'a1_ingest',
  'Two fixture files have been placed in the sandbox:

  incoming/customers.csv
  incoming/orders.json

Step 1: Read incoming/customers.csv with read_file. The first line is a header (skip it).
Insert each remaining row into ingest.customers using sql_exec.

Step 2: Read incoming/orders.json with read_file. It is a JSON array of objects.
Insert each object into ingest.orders using sql_exec.

Step 3: Query both tables with sql_query and report the final row counts.'
);
```

**Expected outcome.** The assertion verifies:

```sql
SELECT
    (SELECT count(*) FROM ingest.customers) = 5
    AND (SELECT count(*) FROM ingest.orders) = 6
    AND NOT EXISTS (
        SELECT 1 FROM ingest.orders o
        WHERE NOT EXISTS (
            SELECT 1 FROM ingest.customers c WHERE c.id = o.customer_id
        )
    )
    AND EXISTS (
        SELECT 1 FROM ingest.customers
        WHERE id = 1 AND name = 'Alice Nguyen'
          AND email = 'alice@example.com' AND country = 'US'
    )
    AND EXISTS (
        SELECT 1 FROM ingest.orders
        WHERE order_id = 104 AND amount = 75.25 AND status = 'shipped'
    )
AS passed;
```

In plain words: 5 customers loaded, 6 orders loaded, every order's
`customer_id` exists in `customers`, spot-checks on specific rows pass.

---

## Agent 2: Feedback Distill (a2_distill)

**What it does.** Reads a table of raw customer feedback, classifies each
item as positive, negative, or neutral, writes a one-line gist per item to
a digest table, then calls `lede_compress` to produce a token-bounded
executive brief.

**Tools used.** `sql_query`, `sql_exec`, `lede_compress`

**Schema.**

```sql
CREATE SCHEMA IF NOT EXISTS feedback;

CREATE TABLE IF NOT EXISTS feedback.items (
    id   INT PRIMARY KEY,
    body TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS feedback.digest (
    id        INT PRIMARY KEY,
    sentiment TEXT NOT NULL,
    gist      TEXT NOT NULL
);
```

Seed populates `feedback.items` with 8 deterministic rows of mixed-sentiment
comments (positive onboarding, negative API confusion, neutral "meets basic
needs", etc.). `feedback.digest` starts empty.

**Sample code.**

```sql
SELECT synapse.llm_profile_set(
  'distill-profile',
  'openai',
  'your-model-name-here',
  'http://your-llm-endpoint:8000/v1',
  NULL,
  '{}'::jsonb
);

SELECT synapse.agent_create(
  'a2_distill',
  $$You are an assistant that analyzes customer feedback and produces an executive digest.

You have access to three tools:
- sql_query: run a SELECT and get rows back as JSON.
- sql_exec: run INSERT, UPDATE, or DELETE. Always use $1, $2, ... placeholders with a params array.
- lede_compress: compress text to a token-bounded brief. Args: text (text to compress), max_tokens (budget, default 200).

Tables:
  feedback.items(id INT, body TEXT)          -- 8 rows of raw customer feedback
  feedback.digest(id INT, sentiment TEXT, gist TEXT) -- starts empty, you populate it

Allowed sentiment labels: exactly one of positive, negative, neutral (lowercase).

Steps:
1. Call sql_query to fetch all rows from feedback.items.
2. For each item, decide its sentiment and write a one-sentence gist.
3. For each item, call sql_exec to INSERT one row into feedback.digest:
   INSERT INTO feedback.digest (id, sentiment, gist) VALUES ($1, $2, $3)
4. After all 8 rows are inserted, concatenate all gists and call lede_compress
   with max_tokens=60 to produce the executive brief.
5. Report the brief.

Insert each feedback item as a separate sql_exec call. Do not batch multiple rows.$$,
  'conversation',
  'distill-profile',
  ARRAY['sql_query', 'sql_exec', 'lede_compress'],
  30,
  120000
);

SELECT synapse.execute(
  'a2_distill',
  'Read all customer feedback items from the feedback.items table. For each item, classify the sentiment as positive, negative, or neutral and write a one-line gist. Insert a digest row for each item into feedback.digest. Then call lede_compress on the concatenated gists with a token budget of 60 to produce an executive brief, and report the brief.'
);
```

**Expected outcome.**

```sql
WITH
expected AS (
    SELECT * FROM (VALUES
        (1,'positive'),(2,'positive'),(3,'negative'),(4,'negative'),
        (5,'positive'),(6,'positive'),(7,'negative'),(8,'neutral')
    ) AS t(id, expected_sentiment)
),
row_count_ok AS (
    SELECT (COUNT(*) = 8) AS ok FROM feedback.digest
),
labels_valid AS (
    SELECT (COUNT(*) = 0) AS ok
    FROM feedback.digest
    WHERE sentiment NOT IN ('positive','negative','neutral')
),
match_count AS (
    SELECT COUNT(*) AS matches
    FROM feedback.digest d
    JOIN expected e ON d.id = e.id AND d.sentiment = e.expected_sentiment
)
SELECT
    (SELECT ok FROM row_count_ok)
    AND (SELECT ok FROM labels_valid)
    AND (SELECT matches >= 6 FROM match_count)
AS passed;
```

In plain words: 8 digest rows written, every sentiment label is valid, at
least 6 of 8 match the known-good labels (loose grading, testing the agent
loop correctness rather than perfect sentiment models).

---

## Agent 3: Support Triage (a3_triage)

**What it does.** Reads open support tickets, joins each to the customer tier,
assigns a category and priority, applies an escalation rule
(enterprise + urgent = escalated), updates the ticket, and writes one audit
row per ticket. Expansion of the `examples/customer-support-triage/` workflow
with a full audit trail.

**Tools used.** `sql_query`, `sql_exec`

**Schema.** At `SCALE=1`, 4 tickets are seeded (one per archetype).

```sql
CREATE SCHEMA IF NOT EXISTS support;

CREATE TABLE IF NOT EXISTS support.customers (
    id    INT PRIMARY KEY,
    email TEXT NOT NULL,
    tier  TEXT NOT NULL CHECK (tier IN ('free','pro','enterprise'))
);

CREATE TABLE IF NOT EXISTS support.tickets (
    id          INT PRIMARY KEY,
    customer_id INT REFERENCES support.customers(id),
    subject     TEXT NOT NULL,
    body        TEXT NOT NULL,
    category    TEXT,
    priority    TEXT,
    escalated   BOOLEAN,
    handled_at  TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS support.audit (
    ticket_id INT,
    action    TEXT,
    at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Fixed customers:

```sql
INSERT INTO support.customers (id, email, tier) VALUES
    (1, 'alice@enterprise-corp.com', 'enterprise'),
    (2, 'bob@prouser.io',            'pro'),
    (3, 'carol@freemail.net',        'free')
ON CONFLICT DO NOTHING;
```

Archetype-based tickets (at SCALE=1, 4 rows):

| ticket | customer | archetype | escalation expected |
|--------|----------|-----------|---------------------|
| 1 | alice (enterprise) | API outage (urgent) | escalated = true |
| 2 | bob (pro) | billing dispute (urgent) | escalated = false |
| 3 | carol (free) | how-to question (low) | escalated = false |
| 4 | alice (enterprise) | dashboard feedback (normal) | escalated = false |

**Sample code.**

```sql
SELECT synapse.llm_profile_set(
  'triage-profile',
  'openai',
  'your-model-name-here',
  'http://your-llm-endpoint:8000/v1',
  NULL,
  '{}'::jsonb
);

SELECT synapse.agent_create(
  'a3_triage',
  $$You are a support triage assistant. Your job is to classify open support tickets,
set priorities, apply escalation rules, and record every decision in an audit trail.

Tools available:
- sql_query: run SELECT statements (read-only)
- sql_exec: run INSERT or UPDATE statements (writes)

Schema:
- support.customers(id int, email text, tier text) -- tier: free, pro, enterprise
- support.tickets(id int, customer_id int, subject text, body text, category text,
    priority text, escalated boolean, handled_at timestamptz)
- support.audit(ticket_id int, action text, at timestamptz)

Allowed values:
- category: api, billing, howto, other
- priority: low, normal, high, urgent

Escalation rule (exact):
  escalated = true  ONLY when tier = 'enterprise' AND priority = 'urgent'
  escalated = false for all other combinations

Per-ticket workflow:
1. sql_query open tickets joined to customer tier (WHERE category IS NULL).
2. For each ticket, decide category and priority from subject + body.
3. Apply the escalation rule precisely.
4. sql_exec UPDATE the ticket:
   UPDATE support.tickets SET category=$1, priority=$2, escalated=$3, handled_at=now() WHERE id=$4
5. sql_exec INSERT one audit row:
   INSERT INTO support.audit (ticket_id, action) VALUES ($1, $2)

After all tickets: sql_query a count grouped by category and report it.
Always use $1, $2, ... placeholders. Never inline literal values.$$,
  'conversation',
  'triage-profile',
  ARRAY['sql_query', 'sql_exec'],
  40,
  120000
);

SELECT synapse.execute(
  'a3_triage',
  'Triage every support ticket where category IS NULL. For each open ticket, use sql_query to fetch it joined to its customer tier, decide category and priority, apply escalation rules, UPDATE the ticket, INSERT an audit row. After all tickets are triaged, report a count grouped by category.'
);
```

**Expected outcome.**

```sql
SELECT
    bool_and(
        t.category IS NOT NULL
        AND t.priority IN ('low','normal','high','urgent')
        AND t.handled_at IS NOT NULL
    )
    AND bool_and(
        t.escalated = (c.tier = 'enterprise' AND t.priority = 'urgent')
    )
    AND (
        (SELECT count(DISTINCT ticket_id) FROM support.audit)
        = (SELECT count(*) FROM support.tickets)
    )
    AND (
        (SELECT count(*) FROM support.audit)
        = (SELECT count(*) FROM support.tickets)
    )
    AS passed
FROM support.tickets t
JOIN support.customers c ON c.id = t.customer_id;
```

In plain words: every ticket has a valid category, priority, and
`handled_at`; the escalation rule is applied correctly in both directions
(enterprise+urgent escalated, everything else not); and exactly one audit row
exists per ticket.

---

## Framework Parity Agents

The three agents below replicate the official quickstart examples from
LangGraph, the OpenAI Agents SDK, and Google ADK using pg_synapse primitives.
Each is a benchmark scenario. The per-framework mapping is documented in
`docs/framework-parity/`.

---

## Agent 4: LangGraph Calculator (lg_calc)

Mirrors the LangGraph quickstart calculator agent
(see `docs/framework-parity/langgraph.md`).

**What it does.** Chains two arithmetic operations using the `calculator`
tool, then stores the result in Postgres. Tests that the agent can correctly
chain tool outputs as inputs to subsequent tool calls.

**Tools used.** `calculator`, `sql_exec`

**Schema.**

```sql
CREATE SCHEMA IF NOT EXISTS lg;

CREATE TABLE IF NOT EXISTS lg.result (
    label TEXT NOT NULL,
    value NUMERIC NOT NULL
);
```

**Sample code.**

```sql
SELECT synapse.llm_profile_set(
  'calc-profile',
  'openai',
  'your-model-name-here',
  'http://your-llm-endpoint:8000/v1',
  NULL,
  '{}'::jsonb
);

SELECT synapse.agent_create(
  'lg_calc',
  $$You are a calculator agent. You have two tools:

1. calculator: performs arithmetic. Arguments: op (add|sub|mul|div), a (first number), b (second number).
   Returns {"result": <number>}.

2. sql_exec: executes SQL. Arguments: query (parameterized SQL), params (array of values for $1, $2, ...).

To complete the task you must:
- Call calculator twice in sequence, chaining the result of the first call as input to the second.
- Then call sql_exec to INSERT the final result into the lg.result table.

Always use parameterized queries:
  query: "INSERT INTO lg.result (label, value) VALUES ($1, $2)"
  params: ["answer", <the number>]$$,
  'conversation',
  'calc-profile',
  ARRAY['calculator', 'sql_exec'],
  10,
  60000
);

SELECT synapse.execute(
  'lg_calc',
  'Compute (12 + 30) then multiply that by 7 using the calculator tool, then sql_exec INSERT the final number into lg.result as label ''answer''.'
);
```

**Expected outcome.**

```sql
SELECT (SELECT value FROM lg.result WHERE label = 'answer') = 294 AS passed;
```

In plain words: the agent computed (12 + 30) = 42, then 42 * 7 = 294, and
stored 294 in `lg.result`.

---

## Agent 5: OpenAI Agents SDK Triage (oai_triage)

Mirrors the OpenAI Agents SDK triage/handoff quickstart
(see `docs/framework-parity/openai-agents-sdk.md`). Uses the `call_agent`
delegation tool as the pg_synapse equivalent of `agent.as_tool()`.

**What it does.** A triage agent receives a question, routes it to the
appropriate specialist sub-agent via `call_agent`, then stores the
specialist's answer. Tests multi-agent delegation within a single Postgres
transaction.

**Tools used.** `call_agent`, `sql_exec` (triage agent); no tools (specialists)

**Schema.**

```sql
CREATE SCHEMA IF NOT EXISTS triage;

CREATE TABLE IF NOT EXISTS triage.log (
    answer TEXT NOT NULL
);
```

The seed also inserts two specialist sub-agent rows directly into
`synapse.agents`: `math_specialist` (answers math questions) and
`history_specialist` (answers history questions). Both use the same LLM
profile as the triage agent and carry an empty tools array (they are leaf
agents that answer directly without further tool calls).

**Sample code.**

```sql
SELECT synapse.llm_profile_set(
  'triage-llm',
  'openai',
  'your-model-name-here',
  'http://your-llm-endpoint:8000/v1',
  NULL,
  '{}'::jsonb
);

-- Specialist sub-agents (the seed inserts these; shown here for clarity).
SELECT synapse.agent_create(
  'math_specialist',
  'You are a math expert. Answer the math question asked of you directly and concisely. Provide just the numeric answer and a brief explanation.',
  'conversation',
  'triage-llm',
  ARRAY[]::text[],
  10,
  120000
);

SELECT synapse.agent_create(
  'history_specialist',
  'You are a history expert. Answer the history question asked of you directly and concisely.',
  'conversation',
  'triage-llm',
  ARRAY[]::text[],
  10,
  120000
);

-- Triage (entry) agent.
SELECT synapse.agent_create(
  'oai_triage',
  $$You are a triage agent that routes questions to the right specialist.

You have two tools:

1. call_agent: delegates to a named sub-agent. Arguments: agent (agent name string), input (question string).
   Returns the specialist's answer as text.
   Available sub-agents: math_specialist (for math questions), history_specialist (for history questions).

2. sql_exec: executes SQL. Arguments: query (parameterized SQL), params (array of values for $1, $2, ...).

Routing rules:
- Math questions (calculations, equations, numbers): call_agent with agent='math_specialist'
- History questions (events, dates, people, places): call_agent with agent='history_specialist'

After routing and receiving the specialist's answer, use sql_exec to INSERT the answer into triage.log:
  query: "INSERT INTO triage.log (answer) VALUES ($1)"
  params: [<specialist answer text>]$$,
  'conversation',
  'triage-llm',
  ARRAY['call_agent', 'sql_exec'],
  15,
  120000
);

SELECT synapse.execute(
  'oai_triage',
  'A student asks: what is 18 times 4? Route it to the right specialist and store the specialist''s answer.'
);
```

**Expected outcome.**

```sql
SELECT (SELECT answer FROM triage.log LIMIT 1) LIKE '%72%' AS passed;
```

In plain words: the triage agent routed the math question to `math_specialist`,
the specialist returned an answer containing "72" (18 x 4 = 72), and the
answer was stored in `triage.log`.

---

## Agent 6: Google ADK Root Agent (adk_root)

Mirrors the Google ADK `root_agent` get-started example
(see `docs/framework-parity/google-adk.md`).

**What it does.** Calls `get_current_time` to retrieve an ISO8601 timestamp,
evaluates whether the returned value is a valid timestamp, and records the
boolean result in Postgres. Tests basic tool use and tool output inspection.

**Tools used.** `get_current_time`, `sql_exec`

**Schema.**

```sql
CREATE SCHEMA IF NOT EXISTS adk;

CREATE TABLE IF NOT EXISTS adk.probe (
    has_time BOOLEAN NOT NULL
);
```

**Sample code.**

```sql
SELECT synapse.llm_profile_set(
  'adk-profile',
  'openai',
  'your-model-name-here',
  'http://your-llm-endpoint:8000/v1',
  NULL,
  '{}'::jsonb
);

SELECT synapse.agent_create(
  'adk_root',
  $$You are a time-aware agent. You have two tools:

1. get_current_time: returns the current time. Arguments: timezone (optional, defaults to UTC).
   Returns {"iso8601": "<RFC3339 timestamp>", "timezone": "<tz>"}.

2. sql_exec: executes SQL. Arguments: query (parameterized SQL), params (array of values for $1, $2, ...).

When asked to get the current time and record whether you received an ISO8601 timestamp:
- Call get_current_time to get the time.
- Check whether the returned iso8601 field looks like a valid ISO8601/RFC3339 timestamp
  (it should start with a 4-digit year and contain a 'T').
- Call sql_exec to INSERT into adk.probe:
  query: "INSERT INTO adk.probe (has_time) VALUES ($1)"
  params: [true]  (or false if the timestamp was missing or malformed)$$,
  'conversation',
  'adk-profile',
  ARRAY['get_current_time', 'sql_exec'],
  8,
  60000
);

SELECT synapse.execute(
  'adk_root',
  'Get the current time with the tool, then sql_exec INSERT into adk.probe whether you received an ISO8601 timestamp (true/false).'
);
```

**Expected outcome.**

```sql
SELECT (SELECT has_time FROM adk.probe LIMIT 1) = true AS passed;
```

In plain words: the agent called `get_current_time`, received a valid
ISO8601 timestamp, and recorded `true` in `adk.probe`.

---

## Reactive Triggers

A Postgres trigger can fire a pg_synapse agent automatically on INSERT or
UPDATE. Two modes are available. They have opposite transactional semantics
by design.

### Queue mode (async, default)

The trigger writes a job row to `synapse.agent_queue` and returns immediately.
The triggering INSERT or UPDATE commits without waiting for the LLM. A drainer
runs the agent later, out of band.

- LLM latency never blocks the writer.
- The agent runs in its own transaction and cannot roll back the row that
  triggered it (that row already committed).
- Use for: enrichment, classification, notification, any case where the write
  is authoritative and the agent reacts after the fact.

### Inline mode (sync, opt-in)

The trigger calls `synapse.execute` synchronously inside the writing
transaction. If the agent errors, or if the agent's output contains
`{"decision":"reject","reason":"..."}`, the trigger raises and the original
INSERT or UPDATE rolls back.

- The agent is a transactional gate: it can veto the write.
- Slow LLM blocks the writer and holds locks for the full duration.
- Use for: validation, policy enforcement, any case where the write must not
  stand if the agent rejects it.

### The queue table

```sql
-- synapse.agent_queue (created by the extension)
job_id      UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
agent       TEXT        NOT NULL,
input       TEXT        NOT NULL,
status      TEXT        NOT NULL DEFAULT 'queued'
                        CHECK (status IN ('queued','running','done','error')),
result      JSONB,
error       TEXT,
source      TEXT,       -- 'trigger:<schema>.<table>'
enqueued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
started_at  TIMESTAMPTZ,
finished_at TIMESTAMPTZ
```

### Attaching a queue-mode trigger

The following example wires `triage_agent` to `demo.tickets`. Every INSERT
enqueues a job; the agent enriches `category` and `priority` when the queue
is drained.

```sql
-- Attach the trigger.
SELECT synapse.attach_agent_trigger(
  'demo.tickets',
  'triage_agent',
  'queue',       -- mode
  'INSERT',      -- events
  NULL,          -- when_sql guard (optional)
  'NEW::text'    -- input_expr passed to the agent
);

-- An INSERT commits immediately; the agent has not run yet.
INSERT INTO demo.tickets (subject, body)
VALUES ('API rate limit exceeded', 'Getting 429 errors since 10 AM.');

-- Inspect the queue.
SELECT job_id, agent, status, enqueued_at
FROM synapse.agent_queue
ORDER BY enqueued_at DESC LIMIT 5;

-- Drain: the agent runs now, enriches the ticket, marks status='done'.
SELECT synapse.drain_queue(10) AS jobs_processed;

-- Schedule with pg_cron for continuous draining.
SELECT cron.schedule('synapse-drain', '* * * * *', 'SELECT synapse.drain_queue(50)');
```

### Attaching an inline-mode trigger (veto gate)

```sql
-- Attach the trigger.
SELECT synapse.attach_agent_trigger(
  'demo.orders',
  'policy_agent',
  'inline',
  'INSERT',
  NULL,
  'NEW::text'
);

-- A bad order is rejected: the INSERT rolls back with the agent's reason.
-- ERROR: synapse inline trigger rejected: amount is negative (fraud risk)
INSERT INTO demo.orders (customer, amount) VALUES ('bad_actor', -50.00);

-- A valid order commits normally.
INSERT INTO demo.orders (customer, amount) VALUES ('alice@acme.com', 150.00);
```

The `policy_agent` system prompt must instruct the model to emit either
`{"decision":"accept"}` or `{"decision":"reject","reason":"<explanation>"}`.
The trigger parses the JSON from the agent's output and raises with the reason
string on reject.

### Recursion guard

Both modes include `IF pg_trigger_depth() > 1 THEN RETURN NEW; END IF;` in
the generated trigger function. If the agent's `sql_exec` writes back to the
same table, the trigger does not re-fire. A `when_sql` row guard provides
an additional production safeguard:

```sql
-- Only fire when the row arrives without a category (avoids re-triggering
-- after the agent writes category back).
SELECT synapse.attach_agent_trigger(
  'demo.tickets',
  'triage_agent',
  'queue',
  'INSERT OR UPDATE',
  'NEW.category IS NULL',
  'NEW::text'
);
```

### Detaching a trigger

```sql
SELECT synapse.detach_agent_trigger('demo.tickets');
```

This drops both the trigger and the generated trigger function.

---

## Running these scenarios

**Toolchain wrapper.** The repository pins `cargo-pgrx` to `0.18.0` and
provides an isolated wrapper to avoid version conflicts with other pgrx trees
on the same machine. Use `./scripts/pgrx` instead of bare `cargo pgrx` for
all build and install commands.

**Benchmark scenarios.** Each of the six agents above maps to a directory
under `bench/scenarios/<name>/`. The directory contains `meta.env` (tool list,
iteration cap), `seed.sql.tmpl` (schema and fixtures), `task.txt` (the user
message), `system_prompt.txt` (the agent prompt), and `assert.sql`
(the pass/fail boolean query). The benchmark harness in `bench/run_bench.sh`
seeds, runs, and asserts across the configured model matrix.

**Standalone examples.** The `examples/` directory contains simpler,
self-contained demos that run against a live pg_synapse installation:

- `examples/reactive-triggers/` -- queue and inline trigger demos with
  `run.sh`, `seed.sql`, `queue_demo.sql`, and `inline_demo.sql`.
- `examples/sql-agent-readwrite/` -- canonical read/write agent demo.
- `examples/customer-support-triage/` -- multi-step ticket triage.
- `examples/with-local-embeddings/` -- local BGE embeddings plus cosine search.

See `docs/AGENTS-GUIDE.md` (this file) for the agent catalog and
`examples/reactive-triggers/` for the reactive trigger demo.
