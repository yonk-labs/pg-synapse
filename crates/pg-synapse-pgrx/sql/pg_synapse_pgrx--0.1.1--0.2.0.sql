-- pg_synapse_pgrx 0.1.1 -> 0.2.0
--
-- Additive only: harness components on synapse.agents, the skills,
-- tool_hints and lessons tables, the trajectory renderer, and seven new
-- functions. No existing function changed signature (checked by diffing the
-- `cargo pgrx schema` output of both versions), so nothing is dropped.
--
-- Apply with: ALTER EXTENSION pg_synapse_pgrx UPDATE TO '0.2.0';
--
-- A database that took the pg-one changes at 0.1.1 ran migrate-f2.sql and
-- migrate-model-tiers.sql by hand. Where that happened, config_agents() is
-- re-created below so it also carries the harness components; where it did
-- not, the block is skipped and CREATE EXTENSION's schema.sql is the source.

-- Always-on harness components.
ALTER TABLE synapse.agents ADD COLUMN IF NOT EXISTS guardrails TEXT;
ALTER TABLE synapse.agents ADD COLUMN IF NOT EXISTS memory     TEXT;
ALTER TABLE synapse.agents ADD COLUMN IF NOT EXISTS critique   TEXT;

-- The brownfield scan state lives on synapse.connections (it was mistakenly
-- declared on llm_profiles in 0.1.1 builds of schema.sql). Guarded because a
-- database that never took the pg-one changes has no connections table.
DO $conn_scan$
BEGIN
  IF to_regclass('synapse.connections') IS NOT NULL THEN
    ALTER TABLE synapse.connections ADD COLUMN IF NOT EXISTS scan_json   JSONB;
    ALTER TABLE synapse.connections ADD COLUMN IF NOT EXISTS scanned_at  TIMESTAMPTZ;
    ALTER TABLE synapse.connections ADD COLUMN IF NOT EXISTS reviewed_at TIMESTAMPTZ;
    ALTER TABLE synapse.connections ADD COLUMN IF NOT EXISTS reviewed_by TEXT;
  END IF;
END
$conn_scan$;

-- Harness components beyond the always-on columns on synapse.agents.
--
-- skills: on-demand procedures. One trigger line per skill is always-on;
-- the body enters context only when the model calls `load_skill`.
CREATE TABLE IF NOT EXISTS synapse.skills (
  agent       TEXT NOT NULL REFERENCES synapse.agents(name) ON DELETE CASCADE,
  name        TEXT NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9_-]*$'),
  description TEXT NOT NULL,
  body        TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (agent, name)
);

-- tool_hints: appended to a failed tool call's message when the error text
-- contains error_pattern (case-insensitive). agent NULL = every agent;
-- tool_name NULL = every tool. Agent-specific rows are consulted first.
CREATE TABLE IF NOT EXISTS synapse.tool_hints (
  hint_id       BIGSERIAL PRIMARY KEY,
  agent         TEXT REFERENCES synapse.agents(name) ON DELETE CASCADE,
  tool_name     TEXT,
  error_pattern TEXT NOT NULL CHECK (length(trim(error_pattern)) > 0),
  hint          TEXT NOT NULL,
  enabled       BOOLEAN NOT NULL DEFAULT true,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- lessons: the experience library. Active lessons are folded into the
-- agent's memory component at load. Every row cites its evidence; the
-- execution it came from is kept when it still exists.
CREATE TABLE IF NOT EXISTS synapse.lessons (
  lesson_id        BIGSERIAL PRIMARY KEY,
  agent            TEXT NOT NULL REFERENCES synapse.agents(name) ON DELETE CASCADE,
  cue              TEXT NOT NULL,
  lesson           TEXT NOT NULL,
  evidence         TEXT NOT NULL,
  confidence       TEXT,
  source_execution UUID REFERENCES synapse.executions(execution_id) ON DELETE SET NULL,
  status           TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','retired')),
  superseded_by    BIGINT REFERENCES synapse.lessons(lesson_id),
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS lessons_agent_active_idx ON synapse.lessons (agent) WHERE status = 'active';

-- Render one execution's messages as a readable trajectory in the
-- [ACTION] / [OBSERVATION] / [ERROR] grammar (borrowed from harness2), so a
-- human or an improver model reads what the agent did, not table rows.
-- Observations longer than p_trunc chars are cut with an explicit marker;
-- the floor of 10000 is where harness2 found shorter cuts start to read as
-- solver failures. Pure SQL, so the sidecar's seeded schema has it too.
CREATE OR REPLACE FUNCTION synapse.trajectory_trunc(t TEXT, n INT)
RETURNS TEXT LANGUAGE sql IMMUTABLE AS $fn$
  SELECT CASE
    WHEN t IS NULL THEN ''
    WHEN n IS NULL OR n <= 0 OR length(t) <= n THEN t
    ELSE left(t, n) || E'\n[... observation truncated by the renderer at ' || n
         || ' chars; ' || (length(t) - n) || ' more not shown. This cut is ours, not the agent''s.]'
  END
$fn$;

CREATE OR REPLACE FUNCTION synapse.trajectory(p_execution_id UUID, p_trunc INT DEFAULT 10000)
RETURNS TEXT LANGUAGE sql STABLE AS $fn$
  WITH header AS (
    SELECT '[EXECUTION] ' || e.execution_id || ' agent=' || e.agent_name
           || ' status=' || e.status
           || ' tokens_in=' || e.tokens_in || ' tokens_out=' || e.tokens_out
           || ' duration_ms=' || coalesce(e.duration_ms::text, '?') AS line
    FROM synapse.executions e WHERE e.execution_id = p_execution_id
  ),
  body AS (
    SELECT m.seq,
      CASE m.role
        WHEN 'system' THEN '[SYSTEM]' || E'\n' || coalesce(m.content, '')
        WHEN 'user' THEN '[USER]' || E'\n' || coalesce(m.content, '')
        WHEN 'assistant' THEN
          CASE WHEN m.tool_name IS NOT NULL
               THEN '[ACTION] ' || m.tool_name || ' ' || coalesce(m.tool_input::text, '{}')
               ELSE '[ASSISTANT]' || E'\n' || coalesce(m.content, '')
          END
        WHEN 'tool' THEN
          CASE WHEN coalesce(m.content, '') LIKE 'ERROR:%'
                 OR (m.tool_output IS NOT NULL AND jsonb_typeof(m.tool_output) = 'object'
                     AND m.tool_output ? 'error')
               THEN '[ERROR] ' || coalesce(m.tool_name, '?') || E'\n'
               ELSE '[OBSERVATION] ' || coalesce(m.tool_name, '?') || E'\n'
          END || synapse.trajectory_trunc(coalesce(m.content, m.tool_output::text, ''), p_trunc)
        ELSE '[' || upper(m.role) || ']' || E'\n' || coalesce(m.content, '')
      END AS line
    FROM synapse.messages m WHERE m.execution_id = p_execution_id
  )
  SELECT coalesce((SELECT line FROM header), '[EXECUTION] ' || p_execution_id || ' (no executions row)')
         || E'\n\n'
         || coalesce((SELECT string_agg(line, E'\n\n' ORDER BY seq) FROM body), '(no messages persisted; raise trace_level)')
$fn$;

-- New functions (bodies generated by pgrx; kept in step with sql_functions.rs).

-- pg_synapse_pgrx::sql_functions::synapse::agent_harness
CREATE  FUNCTION synapse."agent_harness"(
	"agent" TEXT /* & str */
) RETURNS jsonb /* JsonB */
STRICT SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'agent_harness_wrapper';

-- pg_synapse_pgrx::sql_functions::synapse::agent_set_component
CREATE  FUNCTION synapse."agent_set_component"(
	"agent" TEXT, /* & str */
	"component" TEXT, /* & str */
	"value" TEXT /* Option < & str > */
) RETURNS void
SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'agent_set_component_wrapper';

-- pg_synapse_pgrx::sql_functions::synapse::lesson_add
CREATE  FUNCTION synapse."lesson_add"(
	"agent" TEXT, /* & str */
	"cue" TEXT, /* & str */
	"lesson" TEXT, /* & str */
	"evidence" TEXT, /* & str */
	"confidence" TEXT, /* Option < & str > */
	"source_execution" uuid /* Option < pgrx :: Uuid > */
) RETURNS bigint /* i64 */
SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'lesson_add_wrapper';

-- pg_synapse_pgrx::sql_functions::synapse::lesson_retire
CREATE  FUNCTION synapse."lesson_retire"(
	"lesson_id" bigint, /* i64 */
	"superseded_by" bigint /* Option < i64 > */
) RETURNS void
SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'lesson_retire_wrapper';

-- pg_synapse_pgrx::sql_functions::synapse::skill_drop
CREATE  FUNCTION synapse."skill_drop"(
	"agent" TEXT, /* & str */
	"name" TEXT /* & str */
) RETURNS void
STRICT SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'skill_drop_wrapper';

-- pg_synapse_pgrx::sql_functions::synapse::skill_set
CREATE  FUNCTION synapse."skill_set"(
	"agent" TEXT, /* & str */
	"name" TEXT, /* & str */
	"description" TEXT, /* & str */
	"body" TEXT /* & str */
) RETURNS void
STRICT SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'skill_set_wrapper';

-- pg_synapse_pgrx::sql_functions::synapse::tool_hint_add
CREATE  FUNCTION synapse."tool_hint_add"(
	"agent" TEXT, /* Option < & str > */
	"tool_name" TEXT, /* Option < & str > */
	"error_pattern" TEXT, /* & str */
	"hint" TEXT /* & str */
) RETURNS bigint /* i64 */
SECURITY DEFINER
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'tool_hint_add_wrapper';

-- The data-driven harness components for one agent: skills, tool hints
-- (agent-specific first, then global) and active lessons. Lives behind the
-- same definer boundary as config_agents, for the same reason, and like the
-- other config_* functions it is owner-only and deliberately never granted.
-- Defined here rather than next to config_agents because a SQL-language body
-- is checked at creation and these tables do not exist yet up there.
CREATE OR REPLACE FUNCTION synapse.config_harness(p_agent text)
RETURNS jsonb LANGUAGE sql SECURITY DEFINER STABLE AS $$
  SELECT jsonb_build_object(
    'skills', COALESCE((
      SELECT jsonb_agg(jsonb_build_object(
               'name', s.name, 'description', s.description, 'body', s.body)
             ORDER BY s.name)
      FROM synapse.skills s WHERE s.agent = p_agent), '[]'::jsonb),
    'tool_hints', COALESCE((
      SELECT jsonb_agg(jsonb_build_object(
               'tool_name', h.tool_name, 'error_pattern', h.error_pattern, 'hint', h.hint)
             ORDER BY (h.agent IS NULL), h.hint_id)
      FROM synapse.tool_hints h
      WHERE h.enabled AND (h.agent = p_agent OR h.agent IS NULL)), '[]'::jsonb),
    'lessons', COALESCE((
      SELECT jsonb_agg(jsonb_build_object('cue', l.cue, 'lesson', l.lesson)
             ORDER BY l.lesson_id)
      FROM synapse.lessons l
      WHERE l.agent = p_agent AND l.status = 'active'), '[]'::jsonb))
$$;

-- config_agents() gains the always-on components, but only where the pg-one
-- shape (model_tier, agent_llm_profile) is already in place; the body is
-- checked at creation and would fail against a plain 0.1.1 agents table.
DO $cfg$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'synapse' AND table_name = 'agents'
               AND column_name = 'model_tier') THEN
    EXECUTE $fn$
      CREATE OR REPLACE FUNCTION synapse.config_agents()
      RETURNS jsonb LANGUAGE sql SECURITY DEFINER STABLE AS $body$
        SELECT COALESCE(jsonb_agg(to_jsonb(a)), '[]'::jsonb) FROM (
          SELECT name, system_prompt, soul, executor_name,
                 synapse.agent_llm_profile(name) AS llm_profile_main,
                 COALESCE(
                   llm_profile_small,
                   NULLIF(current_setting('pg_synapse.default_llm_profile_small', true), '')
                 ) AS llm_profile_small,
                 llm_profile_judge, embedding_profile, tools,
                 max_iterations, timeout_ms, cost_cap_usd, trace_level, model_tier,
                 jsonb_build_object('guardrails', guardrails,
                                    'memory',     memory,
                                    'critique',   critique) AS components
          FROM synapse.agents) a
      $body$;
    $fn$;
  END IF;
END $cfg$;

-- Grants, mirroring grants.sql. New functions start with PUBLIC EXECUTE, so
-- strip it first, as the fresh-install finalize step does.
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA synapse FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA synapse FROM PUBLIC;
GRANT SELECT ON synapse.skills     TO synapse_user;
GRANT SELECT ON synapse.tool_hints TO synapse_user;
GRANT SELECT ON synapse.lessons    TO synapse_user;
GRANT SELECT, INSERT, UPDATE, DELETE ON synapse.skills, synapse.tool_hints, synapse.lessons TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.agent_set_component(text, text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.skill_set(text, text, text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.skill_drop(text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.tool_hint_add(text, text, text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.lesson_add(text, text, text, text, text, uuid) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.lesson_retire(bigint, bigint) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.trajectory(uuid, integer) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.trajectory(uuid, integer) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.trajectory_trunc(text, integer) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.trajectory_trunc(text, integer) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.agent_harness(text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.agent_harness(text) TO synapse_admin;
