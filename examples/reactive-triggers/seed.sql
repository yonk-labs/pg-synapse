-- Seed schema + demo tables for the reactive-triggers example.
-- Registers an LLM profile pointing at the local vLLM endpoint and two agents:
--   triage_agent   - enriches rows after they commit (queue mode)
--   policy_agent   - gates rows before they commit (inline mode)
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;

-- Demo application schema.
CREATE SCHEMA IF NOT EXISTS demo;

-- Tickets table for the queue-mode demo.
-- The triage_agent enriches category and priority after the row commits.
CREATE TABLE IF NOT EXISTS demo.tickets (
  id          SERIAL PRIMARY KEY,
  subject     TEXT NOT NULL,
  body        TEXT NOT NULL,
  category    TEXT,
  priority    TEXT,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Orders table for the inline-mode demo.
-- The policy_agent gates each INSERT; rows with a negative amount are rejected.
CREATE TABLE IF NOT EXISTS demo.orders (
  id          SERIAL PRIMARY KEY,
  customer    TEXT NOT NULL,
  amount      NUMERIC(12,2) NOT NULL,
  status      TEXT NOT NULL DEFAULT 'pending',
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- LLM profile pointing at the local vLLM instance.
-- Override with PG_SYNAPSE_LLM_BASE_URL / PG_SYNAPSE_LLM_MODEL env vars
-- (the run.sh script does a sed pass before applying this file).
SELECT synapse.llm_profile_set(
  'vllm-default',
  'openai',
  'Intel/Qwen3-Coder-Next-int4-AutoRound',
  'http://192.168.1.193:8000/v1',
  NULL,
  '{}'::jsonb
);

-- Queue-mode agent: classifies a ticket and writes back category + priority.
SELECT synapse.agent_create(
  'triage_agent',
  $$You are a ticket triage assistant.

Given a raw ticket row (as text), classify it and update the row.
The ticket row looks like: (id, subject, body, category, priority, created_at).

Steps:
1. Extract the ticket id from the input text.
2. Call sql_query to get the full ticket:
   query: SELECT id, subject, body FROM demo.tickets WHERE id = $1
   params: [<id>]
3. Decide: category in {api, billing, account, howto, other}
           priority in {low, normal, high, urgent}
4. Call sql_exec to write back:
   query: UPDATE demo.tickets SET category = $1, priority = $2 WHERE id = $3
   params: ["<category>", "<priority>", <id>]
5. Reply with one line: "Ticket <id>: category=<cat> priority=<prio>"

Always use $1, $2, ... placeholders. Never inline values in SQL.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  6,
  90000
);

-- Inline-mode agent: policy gate for orders.
-- Emit {"decision":"reject","reason":"..."} to veto a bad row,
-- or {"decision":"accept"} to let it through.
SELECT synapse.agent_create(
  'policy_agent',
  $$You are a financial policy gate.

You receive a new order row as text. Your job is to decide whether to accept or reject it.

Rules:
- Reject if amount is negative (fraud risk).
- Reject if amount is zero (invalid order).
- Accept otherwise.

You MUST respond with ONLY a JSON object on a single line:
  {"decision":"accept"}
  or
  {"decision":"reject","reason":"<short explanation>"}

Do not include any other text. Do not use markdown. Output only the JSON.$$,
  'conversation',
  'vllm-default',
  ARRAY[]::text[],
  3,
  30000
);
