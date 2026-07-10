-- Scenario: the DBA that opens tickets (safe recommendations).
-- Seeds a health-signals table (synthetic monitoring findings) plus a
-- recommendations ticket table. The agent auto-applies only what is safe
-- inside its transaction (plain CREATE INDEX, ANALYZE, DML) and files a
-- ticket for everything that is not (ALTER SYSTEM, REINDEX/CREATE INDEX
-- CONCURRENTLY, VACUUM, memory changes).
-- Reload-safe: reloading drops and recreates the dba tables.
-- Assumes the UI has already configured the 'vllm-default' LLM profile.
--
-- Expected end state (assert probe): every signal resolved, at least three
-- tickets filed, and the audit_log actor_id index created.

CREATE SCHEMA IF NOT EXISTS dba;

DROP TABLE IF EXISTS dba.recommendations;
DROP TABLE IF EXISTS dba.health_signals;
DROP TABLE IF EXISTS dba.audit_log;

CREATE TABLE dba.audit_log (
  id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  actor_id  INT NOT NULL,
  action    TEXT NOT NULL,
  logged_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO dba.audit_log (actor_id, action)
SELECT (random() * 500)::int, 'login'
FROM generate_series(1, 5000);

CREATE TABLE dba.health_signals (
  id          SERIAL PRIMARY KEY,
  signal      TEXT NOT NULL,
  detail      TEXT NOT NULL,
  resolved    BOOLEAN NOT NULL DEFAULT false,
  captured_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE dba.recommendations (
  id             SERIAL PRIMARY KEY,
  signal_id      INT REFERENCES dba.health_signals(id),
  severity       TEXT,
  recommendation TEXT NOT NULL,
  rationale      TEXT,
  requires_human BOOLEAN NOT NULL DEFAULT true,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO dba.health_signals (signal, detail) VALUES
  ('sort_spill',
   'Nightly revenue report sorts are spilling to disk; temp files peaked at 1.2 GB last night. work_mem is 4MB.'),
  ('index_bloat',
   'Index public.events_pkey is estimated 68 percent bloated; a REINDEX during low-traffic hours is recommended.'),
  ('low_cache_hit',
   'Buffer cache hit ratio fell to 82 percent (target 95+). shared_buffers is 128MB on a 64GB host.'),
  ('unindexed_scan',
   'dba.audit_log is sequentially scanned about 40k times per day filtering on actor_id; no index exists on that column.');

SELECT synapse.agent_create(
  'dba_advisor',
  $$You are a careful senior Postgres DBA running INSIDE a database
transaction via SPI.

Hard rule: some actions cannot run in a transaction block or need a human /
restart, so you must NEVER attempt them: ALTER SYSTEM, VACUUM, REINDEX,
REINDEX CONCURRENTLY, CREATE INDEX CONCURRENTLY, changing work_mem or
shared_buffers, adding memory. For those you file a ticket instead.

Safe to auto-apply right now: plain CREATE INDEX, ANALYZE, and ordinary
INSERT/UPDATE/DELETE.

Workflow:
1. Read pending signals. Call sql_query with
   query: SELECT id, signal, detail FROM dba.health_signals WHERE resolved = false ORDER BY id
   params: []
2. For each signal decide: auto-fix or ticket.
   - Auto-fix example: CREATE INDEX idx_audit_log_actor_id ON dba.audit_log (actor_id)
     via sql_exec with params: []
   - Ticket: call sql_exec with
     query: INSERT INTO dba.recommendations (signal_id, severity, recommendation, rationale, requires_human) VALUES ($1, $2, $3, $4, true)
     params: [<signal id>, "<low|medium|high>", "<one-line action>", "<why it needs a human>"]
3. Mark every handled signal resolved. Call sql_exec with
   query: UPDATE dba.health_signals SET resolved = true WHERE id = $1
   params: [<signal id>]
4. Reply with one line per signal: "AUTO-FIXED: ..." or "TICKETED: ..." and
   the reason.

Always pass values through the params array with $1, $2, ... placeholders.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  15,
  180000
);

SELECT synapse.agent_set_trace_level('dba_advisor', 'debug');
