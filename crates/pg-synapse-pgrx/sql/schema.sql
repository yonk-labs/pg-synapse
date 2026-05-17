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
  created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

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

GRANT USAGE ON SCHEMA synapse TO synapse_admin;
GRANT USAGE ON SCHEMA synapse TO synapse_user;
GRANT SELECT ON synapse.executions TO synapse_user;
GRANT SELECT ON synapse.messages   TO synapse_user;
GRANT SELECT ON synapse.traces     TO synapse_user;
GRANT SELECT ON synapse.agent_queue TO synapse_user;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA synapse TO synapse_admin;
