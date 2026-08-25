-- Scenario: autonomous index tuner (the star for a DBA audience).
-- Seeds perf.orders with 100k rows and NO index on customer_id, so the
-- canonical query does a Seq Scan. The agent diagnoses with EXPLAIN, creates
-- the missing index (plain CREATE INDEX is transaction-safe), and verifies.
-- Reload-safe: reloading drops and recreates the table, so the demo resets.
-- Assumes the UI has already configured the 'vllm-default' LLM profile.
--
-- Expected end state (assert probe): an index on perf.orders(customer_id)
-- exists and the canonical query plans an Index/Bitmap scan.

CREATE SCHEMA IF NOT EXISTS perf;

DROP TABLE IF EXISTS perf.orders;
CREATE TABLE perf.orders (
  id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  customer_id INT NOT NULL,
  amount      NUMERIC(12,2) NOT NULL,
  status      TEXT NOT NULL DEFAULT 'shipped',
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO perf.orders (customer_id, amount)
SELECT (random() * 20000)::int, round((random() * 500)::numeric, 2)
FROM generate_series(1, 100000);

ANALYZE perf.orders;

-- The sql_query tool wraps every query as `SELECT to_jsonb(t) FROM (...) t`,
-- and EXPLAIN is not valid as a subquery. This helper turns a plan into rows
-- so the agent can read it through sql_query. Demo-only: it EXECUTEs its
-- argument (under an EXPLAIN prefix).
CREATE OR REPLACE FUNCTION perf.explain_query(q text) RETURNS SETOF text
LANGUAGE plpgsql AS $fn$
DECLARE line text;
BEGIN
  FOR line IN EXECUTE 'EXPLAIN (ANALYZE, BUFFERS) ' || q LOOP
    RETURN NEXT line;
  END LOOP;
END
$fn$;

SELECT synapse.agent_create(
  'index_tuner',
  $$You are a Postgres performance engineer working inside a single database
transaction. A query on perf.orders is slow; diagnose it and, if the cause is a
missing index, fix it. Only transaction-safe DDL is allowed: a plain CREATE
INDEX is fine, but CREATE INDEX CONCURRENTLY, VACUUM, and server config changes
are NOT (they cannot run in a transaction block).

How to work:
- Read a query's plan through the helper perf.explain_query('<sql text>'),
  which returns the EXPLAIN (ANALYZE, BUFFERS) output as rows. Call it via
  sql_query (EXPLAIN cannot run as a subquery, which is what sql_query needs).
- Look at the existing indexes on the table before adding one (pg_indexes).
- If the plan shows a sequential scan driven by a filter on an unindexed
  column, decide which column needs an index and create a btree index on it.
  Then re-read the plan to confirm it now uses the index.
- Finish with a short before/after: the scan type and timing before, and after.

Work only with perf.orders. Run ONE statement per tool call and never end a
statement with a semicolon. Pass any values through the params array ($1, $2).$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  12,
  120000
);

SELECT synapse.agent_set_trace_level('index_tuner', 'debug');
