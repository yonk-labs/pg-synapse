-- Scenario: customer support triage.
-- Adapted from examples/customer-support-triage/{seed,workflow}.sql.
-- Assumes the UI has already configured the 'vllm-default' LLM profile.
-- Reload-safe: tables use IF NOT EXISTS and the ticket seed only inserts
-- when the table is empty.

CREATE SCHEMA IF NOT EXISTS support;

CREATE TABLE IF NOT EXISTS support.customers (
  id          SERIAL PRIMARY KEY,
  email       TEXT UNIQUE NOT NULL,
  tier        TEXT NOT NULL CHECK (tier IN ('free','pro','enterprise')),
  signed_up   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS support.tickets (
  id          SERIAL PRIMARY KEY,
  customer_id INT REFERENCES support.customers(id),
  subject     TEXT NOT NULL,
  body        TEXT NOT NULL,
  category    TEXT,
  priority    TEXT,
  escalated   BOOLEAN NOT NULL DEFAULT false,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO support.customers (email, tier) VALUES
  ('alice@acme.com',     'enterprise'),
  ('bob@example.org',    'pro'),
  ('charlie@gmail.com',  'free')
ON CONFLICT (email) DO NOTHING;

INSERT INTO support.tickets (customer_id, subject, body)
SELECT c.id, s.subject, s.body
FROM (VALUES
  ('alice@acme.com',    'API is down',
   'Our production API integration started returning 500 errors. URGENT.'),
  ('bob@example.org',   'Billing question',
   'I was charged twice last month. Can you check?'),
  ('charlie@gmail.com', 'How do I export?',
   'Where is the export button in the dashboard?')
) AS s(email, subject, body)
JOIN support.customers c ON c.email = s.email
WHERE NOT EXISTS (SELECT 1 FROM support.tickets);

SELECT synapse.agent_create(
  'triage_agent',
  $$You are a support triage assistant. For each ticket id given:

1. Call sql_query with
   query: SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = $1
   params: [<ticket id>]
2. Decide category in {api, billing, account, howto, other} and priority in {low, normal, high, urgent}. Enterprise tier urgent issues escalate (escalated=true). All others do not escalate.
3. Call sql_exec with
   query: UPDATE support.tickets SET category=$1, priority=$2, escalated=$3 WHERE id=$4
   params: ["<cat>", "<prio>", <bool>, <ticket id>]
4. Reply with a one-line summary of what you did.

Always pass values through the params array using $1, $2, ... placeholders.
Never inline literal values into the SQL string; parameter binding is the
supported and injection-safe path.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  6,
  90000
);

SELECT synapse.agent_set_trace_level('triage_agent', 'debug');
