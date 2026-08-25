-- Scenario: sales lead scoring. Seeds a table of inbound leads with messy
-- free-text notes; the agent reads each, judges fit/urgency, and writes a
-- score (hot/warm/cold) plus a one-line reason. Business value: an SDR's
-- prioritization pass, done in the database.
-- Reload-safe. Assumes the UI configured the 'vllm-default' LLM profile.

CREATE SCHEMA IF NOT EXISTS sales;

DROP TABLE IF EXISTS sales.leads;
CREATE TABLE sales.leads (
  id         SERIAL PRIMARY KEY,
  company    TEXT NOT NULL,
  note       TEXT NOT NULL,
  score      TEXT,
  reason     TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO sales.leads (company, note) VALUES
  ('ACME Corp', 'Enterprise, ~5000 employees. Budget approved, wants to buy the pro tier this quarter, asked for a demo and security review.'),
  ('TinyStartup', 'Solo founder just poking around. No budget, asked if there is a free plan, timeline "someday".'),
  ('Globex', 'Mid-market, ~400 seats. Actively evaluating us against a competitor, decision expected in about two months.'),
  ('Initech', 'Existing customer. Exec sponsor wants to expand to a second team next month, called it urgent.'),
  ('MehCo', 'Downloaded a whitepaper, no reply to three follow-up emails, unclear whether they are even a fit.');

SELECT synapse.agent_create(
  'sales_lead_scorer',
  $$You are a sales development rep. You score inbound leads so the team works
the best ones first.

The data:
- sales.leads(id, company, note, score, reason). note is free text; score and
  reason start empty.

How to work:
- Find the leads that do not have a score yet.
- For each, read the note and judge intent, budget, timeline, and seniority.
  Assign a score of exactly one of: hot, warm, cold. Hot = clear budget and
  near-term intent; warm = real interest but slower or still evaluating; cold =
  no budget, no timeline, or poor fit.
- Write the score and a short one-line reason back onto that lead row.
- Finish with one line per lead: "<company>: <score> - <reason>".

Pass values through the params array ($1, $2, ...); never inline them. Run ONE
statement per tool call and never end it with a semicolon.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  12,
  120000
);

SELECT synapse.agent_set_trace_level('sales_lead_scorer', 'debug');
