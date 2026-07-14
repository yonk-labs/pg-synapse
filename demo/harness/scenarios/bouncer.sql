-- Scenario: transaction bouncer (reactive triggers, both modes).
-- Adapted from examples/reactive-triggers/seed.sql. The queue-mode agent is
-- named enrich_agent here (the triage scenario owns the name triage_agent).
-- Assumes the UI has already configured the 'vllm-default' LLM profile.

CREATE SCHEMA IF NOT EXISTS demo;

-- Queue-mode target: tickets get enriched after commit.
CREATE TABLE IF NOT EXISTS demo.tickets (
  id          SERIAL PRIMARY KEY,
  subject     TEXT NOT NULL,
  body        TEXT NOT NULL,
  category    TEXT,
  priority    TEXT,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Inline-mode target: orders are gated before commit.
CREATE TABLE IF NOT EXISTS demo.orders (
  id          SERIAL PRIMARY KEY,
  customer    TEXT NOT NULL,
  amount      NUMERIC(12,2) NOT NULL,
  status      TEXT NOT NULL DEFAULT 'pending',
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Queue-mode agent: classifies a ticket and writes back category + priority.
SELECT synapse.agent_create(
  'enrich_agent',
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

SELECT synapse.agent_set_trace_level('enrich_agent', 'debug');
SELECT synapse.agent_set_trace_level('policy_agent', 'debug');

-- Attach both triggers up front so the scenario is one click from live.
SELECT synapse.attach_agent_trigger(
  'demo.tickets', 'enrich_agent', 'queue', 'INSERT', NULL, 'NEW::text');
SELECT synapse.attach_agent_trigger(
  'demo.orders', 'policy_agent', 'inline', 'INSERT', NULL, 'NEW::text');
