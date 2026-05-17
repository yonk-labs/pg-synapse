-- Queue-mode reactive trigger demo.
--
-- What this shows:
--   1. Attach a queue-mode trigger to demo.tickets.
--   2. INSERT a ticket row - it commits immediately (no LLM latency).
--   3. Inspect synapse.agent_queue - the job is there with status='queued'.
--   4. Run synapse.drain_queue(10) - the agent runs and enriches the ticket.
--   5. Inspect the result: the ticket now has category + priority.
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;
--   2. \i examples/reactive-triggers/seed.sql

\echo '--- Attaching queue-mode trigger to demo.tickets ---'
SELECT synapse.attach_agent_trigger(
  'demo.tickets',
  'triage_agent',
  'queue',
  'INSERT',
  NULL,
  'NEW::text'
);

\echo ''
\echo '--- Inserting a ticket (commits immediately, no LLM wait) ---'
INSERT INTO demo.tickets (subject, body) VALUES
  ('API rate limit exceeded', 'Getting 429 errors on all endpoints since 10 AM. Production is impacted.');

\echo ''
\echo '--- demo.tickets right after INSERT (category/priority are NULL) ---'
SELECT id, subject, category, priority FROM demo.tickets ORDER BY id;

\echo ''
\echo '--- synapse.agent_queue (job is queued, not yet run) ---'
SELECT job_id, agent, status, source, enqueued_at
FROM synapse.agent_queue
ORDER BY enqueued_at DESC
LIMIT 5;

\echo ''
\echo '--- Running synapse.drain_queue(10) (LLM runs now) ---'
SELECT synapse.drain_queue(10) AS jobs_processed;

\echo ''
\echo '--- synapse.agent_queue after drain (status=done) ---'
SELECT job_id, agent, status, error, finished_at
FROM synapse.agent_queue
ORDER BY enqueued_at DESC
LIMIT 5;

\echo ''
\echo '--- demo.tickets after drain (category + priority enriched) ---'
SELECT id, subject, category, priority FROM demo.tickets ORDER BY id;

\echo ''
\echo '--- Detaching the trigger ---'
SELECT synapse.detach_agent_trigger('demo.tickets');
