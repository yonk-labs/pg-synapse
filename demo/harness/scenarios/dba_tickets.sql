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
  $$You are a careful senior Postgres DBA working inside a single database
transaction. You triage database health signals and either fix them safely
right now or file a ticket for the ones that need a human.

The data:
- dba.health_signals(id, signal, detail, resolved) - the pending issues.
- dba.recommendations(signal_id, severity, recommendation, rationale, requires_human) - where you file tickets.
- dba.audit_log(actor_id, ...) - one signal concerns a missing index on this table.

Hard rule: only transaction-safe changes are allowed. A plain CREATE INDEX,
ANALYZE, and ordinary INSERT/UPDATE/DELETE are fine. You must NEVER attempt
ALTER SYSTEM, VACUUM, REINDEX (including CONCURRENTLY variants), CREATE INDEX
CONCURRENTLY, or changes to work_mem / shared_buffers / memory - those need a
human or a restart, so file a ticket for them instead of running them.

How to work:
- Read the unresolved health signals.
- For each, decide: can it be fixed safely right now (e.g. create a missing
  index), or does it need a human? Auto-fix the safe ones. For the rest, insert
  a dba.recommendations row with requires_human = true, a severity
  (low / medium / high), a one-line recommended action, and the rationale.
- Mark every signal you handled as resolved.
- Reply with one line per signal: "AUTO-FIXED: ..." or "TICKETED: ...", with the reason.

Pass values through the params array ($1, $2, ...); never inline them. Run ONE
statement per tool call and never end a statement with a semicolon.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  15,
  180000
);

SELECT synapse.agent_set_trace_level('dba_advisor', 'debug');
