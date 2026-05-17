-- Inline-mode reactive trigger demo.
--
-- What this shows:
--   1. Attach an inline-mode trigger to demo.orders using policy_agent.
--   2. INSERT a bad order (negative amount) - the INSERT rolls back with the
--      agent's rejection reason surfaced as a Postgres error message.
--   3. INSERT a good order - the INSERT commits normally.
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;
--   2. \i examples/reactive-triggers/seed.sql

\echo '--- Attaching inline-mode trigger to demo.orders ---'
SELECT synapse.attach_agent_trigger(
  'demo.orders',
  'policy_agent',
  'inline',
  'INSERT',
  NULL,
  'NEW::text'
);

\echo ''
\echo '--- Attempting to INSERT a bad order (amount = -50) ---'
\echo '--- Expect: ERROR from the policy agent ---'

-- Wrap in a DO block so psql continues after the expected error.
DO $$
BEGIN
  INSERT INTO demo.orders (customer, amount) VALUES ('bad_actor', -50.00);
  RAISE NOTICE 'ERROR: bad order was NOT rejected (unexpected)';
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'GOOD: bad order was rejected as expected. Reason: %', SQLERRM;
END
$$;

\echo ''
\echo '--- demo.orders after failed INSERT (should be empty) ---'
SELECT id, customer, amount, status FROM demo.orders ORDER BY id;

\echo ''
\echo '--- Inserting a valid order (amount = 150.00) ---'
INSERT INTO demo.orders (customer, amount) VALUES ('alice@acme.com', 150.00);

\echo ''
\echo '--- demo.orders after good INSERT (one row committed) ---'
SELECT id, customer, amount, status FROM demo.orders ORDER BY id;

\echo ''
\echo '--- Detaching the trigger ---'
SELECT synapse.detach_agent_trigger('demo.orders');
