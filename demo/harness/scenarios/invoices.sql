-- Scenario: accounts-receivable collections. Seeds open invoices, some past
-- due; the agent finds the overdue ones, marks them overdue, and logs a
-- short, tone-appropriate reminder message for each. Business value: an AR
-- clerk's daily pass. Reload-safe. Assumes the 'vllm-default' LLM profile.

CREATE SCHEMA IF NOT EXISTS finance;

DROP TABLE IF EXISTS finance.collection_messages;
DROP TABLE IF EXISTS finance.invoices;

CREATE TABLE finance.invoices (
  id           SERIAL PRIMARY KEY,
  customer     TEXT NOT NULL,
  amount       NUMERIC(12,2) NOT NULL,
  due_date     DATE NOT NULL,
  status       TEXT NOT NULL DEFAULT 'open'
);

-- The thread of reminders sent to a customer, oldest first. Separate from
-- invoices so a customer can accumulate more than one message over time
-- (the UI renders this as a chat-style thread in the customer detail modal).
CREATE TABLE finance.collection_messages (
  id         SERIAL PRIMARY KEY,
  invoice_id INT NOT NULL REFERENCES finance.invoices(id) ON DELETE CASCADE,
  message    TEXT NOT NULL,
  sent_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Dates are relative to today so the demo always has a realistic mix.
INSERT INTO finance.invoices (customer, amount, due_date, status) VALUES
  ('ACME Corp',    12500.00, CURRENT_DATE - 45, 'open'),
  ('Globex',        3200.00, CURRENT_DATE - 10, 'open'),
  ('Initech',        890.00, CURRENT_DATE -  2, 'open'),
  ('TinyStartup',    150.00, CURRENT_DATE +  7, 'open'),
  ('Umbrella Co',  48000.00, CURRENT_DATE - 90, 'open'),
  ('Wayne Ent',     2750.00, CURRENT_DATE + 14, 'open');

SELECT synapse.agent_create(
  'collections_agent',
  $$You are an accounts-receivable clerk. You chase overdue invoices without
annoying customers who are still within terms.

The data:
- finance.invoices(id, customer, amount, due_date, status). status starts
  'open'. Today is CURRENT_DATE. These are the ONLY columns; do not assume
  any others exist.
- finance.collection_messages(id, invoice_id, message, sent_at): the log of
  reminders sent to a customer about an invoice. These are the ONLY columns;
  do not add customer_name, amount, days_overdue, or any other column that
  is not listed here. Insert with exactly:
  INSERT INTO finance.collection_messages (invoice_id, message) VALUES ($1, $2)
  Put the customer's name, the amount, and how overdue they are directly in
  the message text, not in separate columns.

How to work:
- Find the open invoices whose due_date is in the past (still unpaid and past
  due). Leave invoices that are not yet due alone.
- For each overdue invoice, in order:
  1. Update its status to 'overdue'.
  2. Insert its reminder message using the exact statement above: a short,
     polite message written directly to the customer, toned to how late they
     are and the amount owed: a gentle nudge when it is a few days late,
     firmer as it ages past 30 and 60 days.
  Do not move on to the next invoice until both steps for the current one
  have succeeded (rows_affected: 1, no error). If a tool call errors, fix and
  retry it before continuing.
- Before you reply, count the overdue invoices you found and the messages you
  inserted this run. They must match. If any invoice is missing its message,
  insert it now, then reply.
- Finish with one line per invoice you actioned: "<customer> $<amount>,
  <days> days overdue".

Compute "days overdue" as CURRENT_DATE - due_date. Pass values through the
params array; never inline them. Run ONE statement per tool call and never end
it with a semicolon.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  14,
  150000
);

SELECT synapse.agent_set_trace_level('collections_agent', 'debug');
