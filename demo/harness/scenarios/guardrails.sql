-- Scenario: guardrails (runaway agent stopped by the runtime).
-- Three agents demonstrate the three stop mechanisms:
--   cost_capped_agent  trips cost_cap_usd (synthetic pricing on a derived profile)
--   time_capped_agent  trips the wall-clock timeout_ms budget
--   marathon_agent     runs long enough for the UI Cancel button
--                      (pg_cancel_backend) to stop it
-- Assumes the UI has already configured the 'vllm-default' LLM profile.

-- Derive a priced profile from the user's endpoint. The synthetic price of
-- $2000 per million tokens makes a few chatty tool-loop turns cost a couple
-- of cents, so the $0.02 cap trips within the first iterations.
SELECT synapse.llm_profile_set(
  'vllm-priced',
  provider,
  model,
  base_url,
  api_key_secret,
  params || '{"cost_per_million_tokens_in": 2000, "cost_per_million_tokens_out": 2000}'::jsonb
)
FROM synapse.llm_profiles
WHERE name = 'vllm-default';

SELECT synapse.agent_create(
  'cost_capped_agent',
  $$You are a deliberately thorough math assistant.

Compute the running sum 1 + 2 + 3 + ... one addition at a time by calling the
calculator tool for every single addition. Never do arithmetic yourself and
never batch additions. After each calculator result, continue with the next
addition. Only stop when told the target number.$$,
  'conversation',
  'vllm-priced',
  ARRAY['calculator'],
  15,
  120000
);

SELECT synapse.agent_create(
  'time_capped_agent',
  $$You are a deliberately thorough math assistant.

Compute the running sum 1 + 2 + 3 + ... one addition at a time by calling the
calculator tool for every single addition. Never do arithmetic yourself and
never batch additions. After each calculator result, continue with the next
addition. Only stop when told the target number.$$,
  'conversation',
  'vllm-default',
  ARRAY['calculator'],
  50,
  4000
);

SELECT synapse.agent_create(
  'marathon_agent',
  $$You are a deliberately thorough math assistant.

Compute the running sum 1 + 2 + 3 + ... one addition at a time by calling the
calculator tool for every single addition. Never do arithmetic yourself and
never batch additions. After each calculator result, continue with the next
addition. Only stop when told the target number.$$,
  'conversation',
  'vllm-default',
  ARRAY['calculator'],
  100,
  300000
);

-- agent_create has no cost cap parameter; the cap lives on the agents row.
UPDATE synapse.agents SET cost_cap_usd = 0.02 WHERE name = 'cost_capped_agent';

SELECT synapse.agent_set_trace_level('cost_capped_agent', 'debug');
SELECT synapse.agent_set_trace_level('time_capped_agent', 'debug');
SELECT synapse.agent_set_trace_level('marathon_agent', 'debug');

-- Pick up the direct UPDATE above.
SELECT synapse.rebuild_kernel();
