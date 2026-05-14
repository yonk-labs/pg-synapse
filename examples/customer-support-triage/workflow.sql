-- Registers an LLM profile and the triage_agent, then runs it against three
-- seeded tickets. The agent reads each ticket + the joined customer tier and
-- writes back category, priority, and an escalated flag.
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;
--   2. \i examples/customer-support-triage/seed.sql

SELECT synapse.llm_profile_set(
  'vllm-default',
  'openai',
  'Intel/Qwen3-Coder-Next-int4-AutoRound',
  'http://192.168.1.193:8000/v1',
  NULL,
  '{}'::jsonb
);

SELECT synapse.agent_create(
  'triage_agent',
  $$You are a support triage assistant. For each ticket id given:

1. Call sql_query with: SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = <ID>.
2. Decide category in {api, billing, account, howto, other} and priority in {low, normal, high, urgent}. Enterprise tier urgent issues escalate (escalated=true). All others do not escalate.
3. Call sql_exec with: UPDATE support.tickets SET category='<cat>', priority='<prio>', escalated=<bool> WHERE id=<ID>.
4. Reply with a one-line summary of what you did.

Inline literal values directly in the SQL. Do not use positional params; that
path is not yet supported by the v0.1-alpha pgrx host.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  6,
  90000
);

SELECT synapse.execute('triage_agent', 'Triage ticket 1.');
SELECT synapse.execute('triage_agent', 'Triage ticket 2.');
SELECT synapse.execute('triage_agent', 'Triage ticket 3.');

-- Inspect what the agent wrote.
SELECT id, subject, category, priority, escalated FROM support.tickets ORDER BY id;
