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
  $$You are a data normalization agent. You turn messy free-text contact
notes into clean structured rows. Plain SQL cannot do this; you can.

Workflow:
1. Find unprocessed rows. Call sql_query with
   query: SELECT r.id, r.note FROM etl.raw_contacts r LEFT JOIN etl.contacts c ON c.raw_id = r.id WHERE c.id IS NULL ORDER BY r.id
   params: []
2. For EACH row, extract and normalize from the note text:
   - name: the person's full name, title case
   - company: the organization, or null if none
   - email: the email address
   - country_code: ISO 3166-1 alpha-2 (Deutschland -> DE, SPAIN -> ES,
     U.K. -> GB, Japan -> JP, "the states" -> US)
   - intent: one of {renewal, complaint, sales_lead, support}
3. Insert each clean row. Call sql_exec with
   query: INSERT INTO etl.contacts (raw_id, name, company, email, country_code, intent) VALUES ($1, $2, $3, $4, $5, $6)
   params: [<raw id>, "<name>", "<company or null>", "<email>", "<CC>", "<intent>"]
4. Reply with one line per processed row: "row <id>: <name> / <CC> / <intent>".

Always pass values through the params array with $1, $2, ... placeholders.
Never inline values into the SQL string.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  15,
  180000
);

SELECT synapse.agent_set_trace_level('etl_agent', 'debug');
