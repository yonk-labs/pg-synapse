-- Model tiers, for a database installed before they existed.
-- schema.sql runs only at CREATE EXTENSION, so a live database needs this.
BEGIN;

ALTER TABLE synapse.agents
  ADD COLUMN IF NOT EXISTS model_tier TEXT NOT NULL DEFAULT 'large';

DO $tier_ck$ BEGIN
  ALTER TABLE synapse.agents ADD CONSTRAINT agents_model_tier_check
    CHECK (model_tier IN ('small','large'));
EXCEPTION WHEN duplicate_object THEN NULL; END $tier_ck$;

CREATE OR REPLACE FUNCTION synapse.agent_llm_profile(p_agent text)
RETURNS text LANGUAGE sql SECURITY DEFINER STABLE AS $$
  SELECT COALESCE(
           a.llm_profile_main,
           CASE WHEN a.model_tier = 'small'
                THEN NULLIF(current_setting('pg_synapse.default_llm_profile_small', true), '')
                ELSE NULLIF(current_setting('pg_synapse.default_llm_profile_main', true), '')
           END)
  FROM synapse.agents a WHERE a.name = p_agent
$$;

CREATE OR REPLACE FUNCTION synapse.config_agents()
RETURNS jsonb LANGUAGE sql SECURITY DEFINER STABLE AS $$
  SELECT COALESCE(jsonb_agg(to_jsonb(a)), '[]'::jsonb) FROM (
    SELECT name, system_prompt, soul, executor_name,
           synapse.agent_llm_profile(name) AS llm_profile_main,
           COALESCE(
             llm_profile_small,
             NULLIF(current_setting('pg_synapse.default_llm_profile_small', true), '')
           ) AS llm_profile_small,
           llm_profile_judge, embedding_profile, tools,
           max_iterations, timeout_ms, cost_cap_usd, trace_level, model_tier
    FROM synapse.agents) a
$$;

CREATE OR REPLACE FUNCTION synapse.record_run(p jsonb)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER AS $record_run$
DECLARE
  v_agent text := p->'execution'->>'agent_name';
  v_exec  uuid := (p->'execution'->>'execution_id')::uuid;
BEGIN
  -- The async path pre-inserts a placeholder row under its own id so a poller
  -- can see the run exists, then the kernel mints a different id for the
  -- messages. Dropping the placeholder here rather than in a separate
  -- statement keeps the replacement atomic: no window in which the run has
  -- two rows, and none in which it has none.
  IF p ? 'supersedes' THEN
    DELETE FROM synapse.executions
     WHERE execution_id = (p->>'supersedes')::uuid;
  END IF;

  INSERT INTO synapse.executions
    (execution_id, agent_name, input, output, status, tokens_in, tokens_out,
     cost_usd, duration_ms, caller_role, model, finished_at)
  SELECT v_exec, v_agent,
         e.input, e.output, e.status, e.tokens_in, e.tokens_out,
         e.cost_usd, e.duration_ms, e.caller_role,
         (SELECT lp.model FROM synapse.llm_profiles lp
           WHERE lp.name = synapse.agent_llm_profile(v_agent)),
         now()
  FROM jsonb_to_record(p->'execution') AS e(
    input text, output text, status text, tokens_in int, tokens_out int,
    cost_usd numeric, duration_ms bigint, caller_role text);

  INSERT INTO synapse.messages
    (execution_id, seq, role, content, tool_call_id, tool_name,
     tool_input, tool_output)
  SELECT v_exec, m.seq, m.role, m.content, m.tool_call_id, m.tool_name,
         m.tool_input, m.tool_output
  FROM jsonb_to_recordset(COALESCE(p->'messages', '[]'::jsonb)) AS m(
    seq int, role text, content text, tool_call_id text, tool_name text,
    tool_input jsonb, tool_output jsonb);

  INSERT INTO synapse.traces (execution_id, seq, event, payload)
  SELECT v_exec, t.seq, t.event, t.payload
  FROM jsonb_to_recordset(COALESCE(p->'events', '[]'::jsonb)) AS t(
    seq int, event text, payload jsonb);
END
$record_run$;

CREATE OR REPLACE FUNCTION synapse.record_status(p jsonb)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER AS $record_status$
DECLARE
  v_agent text := p->>'agent_name';
  v_exec  uuid := (p->>'execution_id')::uuid;
BEGIN
  INSERT INTO synapse.executions
    (execution_id, agent_name, input, output, status, caller_role, model,
     finished_at)
  VALUES (v_exec, v_agent, COALESCE(p->>'input', ''), p->>'output',
          p->>'status', p->>'caller_role',
          (SELECT lp.model FROM synapse.llm_profiles lp
            WHERE lp.name = synapse.agent_llm_profile(v_agent)),
          CASE WHEN p->>'status' = 'queued' THEN NULL ELSE now() END)
  ON CONFLICT (execution_id) DO UPDATE
    SET status      = EXCLUDED.status,
        output      = EXCLUDED.output,
        finished_at = EXCLUDED.finished_at;
END
$record_status$;

-- The tier setter is a Rust #[pg_extern], so a live database also needs its
-- declaration; hot swapping the .so does not create SQL functions.
CREATE OR REPLACE FUNCTION synapse."agent_set_model_tier"("name" TEXT, "tier" TEXT)
RETURNS void STRICT SECURITY DEFINER LANGUAGE c
AS '$libdir/pg_synapse_pgrx', 'agent_set_model_tier_wrapper';

GRANT EXECUTE ON FUNCTION synapse.agent_llm_profile(text) TO synapse_user, synapse_admin;

COMMIT;
