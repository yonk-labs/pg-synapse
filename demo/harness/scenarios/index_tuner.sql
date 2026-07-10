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
  $$You are a Postgres performance engineer working INSIDE a database
transaction. You diagnose a slow query with EXPLAIN and fix it when a missing
index is the cause. Plain CREATE INDEX is allowed; CREATE INDEX CONCURRENTLY,
VACUUM, and config changes are NOT (they cannot run in a transaction block).

Workflow:
1. Get the current plan. Call sql_query with
   query: SELECT * FROM perf.explain_query($1)
   params: ["<the slow query text>"]
2. Check existing indexes. Call sql_query with
   query: SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = 'perf' AND tablename = 'orders'
   params: []
3. If the plan shows a Seq Scan caused by a missing index on the filtered
   column, create it. Call sql_exec with
   query: CREATE INDEX idx_orders_customer_id ON perf.orders (customer_id)
   params: []
4. Verify: repeat step 1 and confirm the plan now uses the index.
5. Reply with a short before/after summary: the scan type and timing before,
   and the scan type and timing after.

Always pass values through the params array with $1, $2, ... placeholders
where a value is needed. Never invent table names; work only with perf.orders.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  8,
  120000
);

SELECT synapse.agent_set_trace_level('index_tuner', 'debug');
