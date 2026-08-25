-- Scenario: LLM-powered ETL (why an agent, not just SQL).
-- Seeds a staging table of messy free-text notes that plain SQL cannot
-- normalize, plus a clean target table. The agent reads the staging rows,
-- uses the model to extract and normalize fields, and writes structured rows
-- to the target: unstructured to structured, entirely inside the database.
-- Reload-safe: reloading drops and recreates the etl tables.
-- Assumes the UI has already configured the 'vllm-default' LLM profile.
--
-- Expected end state (assert probe): every raw row has a matching clean row.

CREATE SCHEMA IF NOT EXISTS etl;

DROP TABLE IF EXISTS etl.contacts;
DROP TABLE IF EXISTS etl.raw_contacts;

CREATE TABLE etl.raw_contacts (
  id          SERIAL PRIMARY KEY,
  note        TEXT NOT NULL,
  received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE etl.contacts (
  id           SERIAL PRIMARY KEY,
  raw_id       INT NOT NULL REFERENCES etl.raw_contacts(id),
  name         TEXT,
  company      TEXT,
  email        TEXT,
  country_code TEXT,
  intent       TEXT,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO etl.raw_contacts (note) VALUES
  ('Call from Bob Smith at ACME Corp (bob.smith@acme.com) -- he is in Deutschland and wants to renew the enterprise contract before Q3.'),
  ('angry voicemail: maria gonzalez, TechFlow S.L., maria@techflow.es ... billing charged twice, she is in SPAIN, wants refund NOW'),
  ('Met Jane Doe (jane.d@globex.co.uk) of Globex at the London meetup, U.K., interested in a demo of the pro tier.'),
  ('fwd: support req / Yuki Tanaka / Nippon Data KK / y.tanaka@nippondata.jp / Japan / cannot export dashboards since last update'),
  ('Prospect: John Q. Public, self-employed, jqp@gmail.com, somewhere in the US (said "the states"), asking about pricing.');

SELECT synapse.agent_create(
  'etl_agent',
  $$You are a data normalization agent. You turn messy free-text contact notes
into clean structured rows, using the model's language understanding to do what
plain SQL cannot.

The data:
- etl.raw_contacts(id, note) holds the messy notes.
- etl.contacts(raw_id, name, company, email, country_code, intent) is the clean
  target; raw_id references the source row.

How to work:
- Find the raw_contacts rows that do not yet have a matching contacts row.
- For each one, read the note and extract: the person's full name (title case),
  the company (or null), the email, the country as an ISO 3166-1 alpha-2 code
  (e.g. Germany -> DE, Spain -> ES, the U.K. -> GB, Japan -> JP, the US -> US),
  and the intent as one of: renewal, complaint, sales_lead, support.
- Write each clean row into etl.contacts. Pass extracted values through the
  params array ($1, $2, ...); never inline them into the SQL string.
- Finish with one short line per processed row: "row <id>: <name> / <CC> / <intent>".

Run ONE statement per tool call and never end a statement with a semicolon.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  15,
  180000
);

SELECT synapse.agent_set_trace_level('etl_agent', 'debug');
