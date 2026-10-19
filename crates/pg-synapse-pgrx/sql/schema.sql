-- pg_synapse_pgrx extension bootstrap: synapse schema, roles, tables, grants.
-- Embedded into the extension via `extension_sql_file!` in src/lib.rs.
--
-- The schema is named `synapse` (not `pg_synapse`) because Postgres reserves
-- the `pg_` prefix for system catalogs. Same reasoning for the role names.

CREATE SCHEMA IF NOT EXISTS synapse;

-- Roles. NOLOGIN by design; ops grant membership to actual users.
DO $bootstrap_roles$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'synapse_admin') THEN
    CREATE ROLE synapse_admin NOLOGIN;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'synapse_user') THEN
    CREATE ROLE synapse_user NOLOGIN;
  END IF;
END
$bootstrap_roles$;

CREATE TABLE IF NOT EXISTS synapse.agents (
  name              TEXT PRIMARY KEY,
  system_prompt     TEXT NOT NULL,
  soul              TEXT,
  executor_name     TEXT NOT NULL DEFAULT 'conversation',
  llm_profile_main  TEXT,
  llm_profile_small TEXT,
  llm_profile_judge TEXT,
  embedding_profile TEXT,
  tools             TEXT[] NOT NULL DEFAULT '{}',
  max_iterations    INT  NOT NULL DEFAULT 10,
  timeout_ms        BIGINT NOT NULL DEFAULT 60000,
  cost_cap_usd      NUMERIC(12,6),
  trace_level       TEXT CHECK (trace_level IN ('off','error','info','debug','full')),
  -- Harness components (always-on, see docs/harness2-borrowings-plan.md).
  guardrails        TEXT,
  memory            TEXT,
  critique          TEXT,
  created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Upgrade path for schemas created before the harness components existed.
ALTER TABLE synapse.agents ADD COLUMN IF NOT EXISTS guardrails TEXT;
ALTER TABLE synapse.agents ADD COLUMN IF NOT EXISTS memory     TEXT;
ALTER TABLE synapse.agents ADD COLUMN IF NOT EXISTS critique   TEXT;

CREATE TABLE IF NOT EXISTS synapse.llm_profiles (
  name             TEXT PRIMARY KEY,
  provider         TEXT NOT NULL,
  model            TEXT NOT NULL,
  api_key_secret   TEXT,
  base_url         TEXT,
  params           JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS synapse.embedding_profiles (
  name             TEXT PRIMARY KEY,
  provider         TEXT NOT NULL,
  model            TEXT NOT NULL,
  dimension        INT  NOT NULL,
  api_key_secret   TEXT,
  base_url         TEXT,
  params           JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS synapse.secrets (
  name        TEXT PRIMARY KEY,
  value       TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS synapse.tools (
  name         TEXT PRIMARY KEY,
  description  TEXT,
  schema_json  JSONB NOT NULL,
  kind         TEXT NOT NULL DEFAULT 'builtin',
  config       JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS synapse.executions (
  execution_id  UUID PRIMARY KEY,
  agent_name    TEXT NOT NULL,
  input         TEXT NOT NULL,
  output        TEXT,
  status        TEXT NOT NULL,
  tokens_in     INT NOT NULL DEFAULT 0,
  tokens_out    INT NOT NULL DEFAULT 0,
  cost_usd      NUMERIC(12,6),
  duration_ms   BIGINT,
  caller_role   TEXT,
  started_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at   TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS synapse.messages (
  execution_id  UUID NOT NULL REFERENCES synapse.executions(execution_id) ON DELETE CASCADE,
  seq           INT  NOT NULL,
  role          TEXT NOT NULL,
  content       TEXT,
  tool_call_id  TEXT,
  tool_name     TEXT,
  tool_input    JSONB,
  tool_output   JSONB,
  ts            TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (execution_id, seq)
);

CREATE TABLE IF NOT EXISTS synapse.traces (
  execution_id  UUID NOT NULL REFERENCES synapse.executions(execution_id) ON DELETE CASCADE,
  seq           INT  NOT NULL,
  event         TEXT NOT NULL,
  payload       JSONB,
  ts            TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (execution_id, seq)
);

-- Reactive triggers: job queue for async agent invocation from triggers.
-- Operator-driven drain (pg_cron or a sidecar poller) runs synapse.drain_queue().
-- A true background worker drain is the v0.2 upgrade (design spec D8).
CREATE TABLE IF NOT EXISTS synapse.agent_queue (
  job_id      UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
  agent       TEXT        NOT NULL,
  input       TEXT        NOT NULL,
  status      TEXT        NOT NULL DEFAULT 'queued'
                          CHECK (status IN ('queued','running','done','error')),
  result      JSONB,
  error       TEXT,
  source      TEXT,
  enqueued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at  TIMESTAMPTZ,
  finished_at TIMESTAMPTZ
);


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

GRANT USAGE ON SCHEMA synapse TO synapse_admin;
GRANT USAGE ON SCHEMA synapse TO synapse_user;
GRANT SELECT ON synapse.executions TO synapse_user;
GRANT SELECT ON synapse.messages   TO synapse_user;
GRANT SELECT ON synapse.traces     TO synapse_user;
GRANT SELECT ON synapse.agent_queue TO synapse_user;
GRANT SELECT ON synapse.skills     TO synapse_user;
GRANT SELECT ON synapse.tool_hints TO synapse_user;
GRANT SELECT ON synapse.lessons    TO synapse_user;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA synapse TO synapse_admin;
