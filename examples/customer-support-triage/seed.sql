-- Seed schema + sample customers + sample tickets for the triage demo.
-- The triage agent will classify each ticket, look up customer tier, and
-- update the ticket row with category, priority, and escalated flag.

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

INSERT INTO support.tickets (customer_id, subject, body) VALUES
  ((SELECT id FROM support.customers WHERE email='alice@acme.com'),
   'API is down',
   'Our production API integration started returning 500 errors. URGENT.'),
  ((SELECT id FROM support.customers WHERE email='bob@example.org'),
   'Billing question',
   'I was charged twice last month. Can you check?'),
  ((SELECT id FROM support.customers WHERE email='charlie@gmail.com'),
   'How do I export?',
   'Where is the export button in the dashboard?')
;
