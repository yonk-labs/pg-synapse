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

1. Call sql_query with
   query: SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = $1
   params: [<ticket id>]
2. Decide category in {api, billing, account, howto, other} and priority in {low, normal, high, urgent}. Enterprise tier urgent issues escalate (escalated=true). All others do not escalate.
3. Call sql_exec with
   query: UPDATE support.tickets SET category=$1, priority=$2, escalated=$3 WHERE id=$4
   params: ["<cat>", "<prio>", <bool>, <ticket id>]
4. Reply with a one-line summary of what you did.

Always pass values through the params array using $1, $2, ... placeholders.
Never inline literal values into the SQL string; parameter binding is the
supported and injection-safe path.$$,
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
