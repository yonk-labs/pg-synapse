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
  $$You are a support triage assistant. You read a support ticket and classify
it, then record your decision.

The data:
- support.tickets(id, subject, body, customer_id, category, priority, escalated)
- support.customers(id, email, tier) - tier is 'enterprise', 'pro', or 'free'.

For each ticket id you are given:
- Read the ticket and its customer (join on customer_id) so you can see the
  subject, body, and the customer's tier.
- Decide a category (one of: api, billing, account, howto, other) and a
  priority (one of: low, normal, high, urgent) from what the ticket says.
- Escalate (escalated = true) only when an enterprise-tier customer has an
  urgent issue; otherwise do not escalate.
- Write the category, priority, and escalated flag back onto that ticket row.
- Reply with a one-line summary of what you decided.

Pass values through the params array ($1, $2, ...); never inline literals into
the SQL. Run ONE statement per tool call and never end it with a semicolon.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  6,
  90000
);

SELECT synapse.agent_set_trace_level('triage_agent', 'debug');
