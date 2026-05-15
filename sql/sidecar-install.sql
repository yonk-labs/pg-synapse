-- sidecar-install.sql: install the synapse schema for the pg-synapse-sidecar host.
--
-- PREREQUISITE: pg_net extension must be installed for the forwarding functions.
--   CREATE EXTENSION IF NOT EXISTS pg_net;
-- Without pg_net, the SECURITY DEFINER forwarder functions cannot make HTTP
-- calls. Fallback options (plpython3u, dblink) are noted in comments only.
--
-- SETUP: Replace {{SIDECAR_URL}} with your sidecar's base URL before running:
--   sed 's|{{SIDECAR_URL}}|http://127.0.0.1:8088|g' sql/sidecar-install.sql | psql -f -
--
-- The sidecar URL placeholder is: {{SIDECAR_URL}}

-- ---------------------------------------------------------------------------
-- Schema and roles
-- ---------------------------------------------------------------------------

CREATE SCHEMA IF NOT EXISTS synapse;

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

-- ---------------------------------------------------------------------------
-- Tables (mirror of crates/pg-synapse-pgrx/sql/schema.sql)
-- ---------------------------------------------------------------------------

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
  max_iterations    INT    NOT NULL DEFAULT 10,
  timeout_ms        BIGINT NOT NULL DEFAULT 60000,
  cost_cap_usd      NUMERIC(12,6),
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

-- ---------------------------------------------------------------------------
-- Role grants (mirror of crates/pg-synapse-pgrx/sql/grants.sql)
-- ---------------------------------------------------------------------------

REVOKE ALL ON SCHEMA synapse FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA synapse FROM PUBLIC;

GRANT USAGE ON SCHEMA synapse TO synapse_admin;
GRANT USAGE ON SCHEMA synapse TO synapse_user;

GRANT SELECT ON synapse.executions TO synapse_user;
GRANT SELECT ON synapse.messages   TO synapse_user;
GRANT SELECT ON synapse.traces     TO synapse_user;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA synapse TO synapse_admin;

REVOKE ALL ON synapse.secrets FROM synapse_user;

-- ---------------------------------------------------------------------------
-- SECURITY DEFINER forwarder functions via pg_net
--
-- These functions forward calls to the sidecar via HTTP using pg_net. Each
-- function calls the appropriate sidecar endpoint and returns the response
-- body as jsonb.
--
-- pg_net.http_post returns a bigint (request id) in older versions and a
-- composite in newer ones. We use a helper approach: call the endpoint and
-- return the response body via pg_net.http_get_result.
--
-- Note: pg_net is required. Alternative approaches (plpython3u, dblink to a
-- loopback) are out of scope for v0.1.1 and are not implemented here.
--
-- The SIDECAR_URL placeholder below is replaced by the sed step documented
-- at the top of this file.
-- ---------------------------------------------------------------------------

-- synapse.version(): return the sidecar version.
CREATE OR REPLACE FUNCTION synapse.version()
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
BEGIN
  SELECT id INTO req_id
  FROM net.http_get('{{SIDECAR_URL}}/v1/version');

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.execute(agent_name text, input text): sync agent execution.
CREATE OR REPLACE FUNCTION synapse.execute(agent_name text, input text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
  body    text;
BEGIN
  body := json_build_object('agent', agent_name, 'input', input)::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/execute',
    body    := body,
    headers := '{"Content-Type":"application/json"}'::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.embed(text_in text, profile text): embed text, return vector as jsonb.
CREATE OR REPLACE FUNCTION synapse.embed(text_in text, profile text DEFAULT NULL)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
  body    text;
BEGIN
  body := json_build_object('text', text_in, 'profile', profile)::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/embed',
    body    := body,
    headers := '{"Content-Type":"application/json"}'::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.execute_async(agent_name text, input text): queue async execution.
-- Returns jsonb with execution_id; poll with synapse.execution_status().
CREATE OR REPLACE FUNCTION synapse.execute_async(agent_name text, input text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
  body    text;
BEGIN
  body := json_build_object('agent', agent_name, 'input', input)::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/execute_async',
    body    := body,
    headers := '{"Content-Type":"application/json"}'::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.execution_status(execution_id uuid): poll async execution.
CREATE OR REPLACE FUNCTION synapse.execution_status(execution_id uuid)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
BEGIN
  SELECT id INTO req_id
  FROM net.http_get('{{SIDECAR_URL}}/v1/status/' || execution_id::text);

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.agent_create(...): create or update an agent via the admin API.
-- Requires X-PG-Synapse-Admin-Token (read from synapse.secrets as 'admin_token').
CREATE OR REPLACE FUNCTION synapse.agent_create(
  p_name            text,
  p_system_prompt   text,
  p_executor_name   text    DEFAULT 'conversation',
  p_llm_profile     text    DEFAULT NULL,
  p_tools           text[]  DEFAULT '{}',
  p_max_iterations  integer DEFAULT 10,
  p_timeout_ms      bigint  DEFAULT 60000
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id   bigint;
  result   jsonb;
  body     text;
  tok      text;
BEGIN
  SELECT value INTO tok FROM synapse.secrets WHERE name = 'admin_token';

  body := json_build_object(
    'name',           p_name,
    'system_prompt',  p_system_prompt,
    'executor_name',  p_executor_name,
    'llm_profile_main', p_llm_profile,
    'tools',          to_json(p_tools),
    'max_iterations', p_max_iterations,
    'timeout_ms',     p_timeout_ms
  )::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/admin/agent',
    body    := body,
    headers := json_build_object(
      'Content-Type', 'application/json',
      'X-PG-Synapse-Admin-Token', coalesce(tok, '')
    )::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.llm_profile_set(...): create or update an LLM profile.
CREATE OR REPLACE FUNCTION synapse.llm_profile_set(
  p_name           text,
  p_provider       text,
  p_model          text,
  p_api_key_secret text    DEFAULT NULL,
  p_base_url       text    DEFAULT NULL,
  p_params         jsonb   DEFAULT '{}'::jsonb
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
  body    text;
  tok     text;
BEGIN
  SELECT value INTO tok FROM synapse.secrets WHERE name = 'admin_token';

  body := json_build_object(
    'name',           p_name,
    'provider',       p_provider,
    'model',          p_model,
    'api_key_secret', p_api_key_secret,
    'base_url',       p_base_url,
    'params',         p_params
  )::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/admin/profile/llm',
    body    := body,
    headers := json_build_object(
      'Content-Type', 'application/json',
      'X-PG-Synapse-Admin-Token', coalesce(tok, '')
    )::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.embedding_profile_set(...): create or update an embedding profile.
CREATE OR REPLACE FUNCTION synapse.embedding_profile_set(
  p_name           text,
  p_provider       text,
  p_model          text,
  p_dimension      integer,
  p_api_key_secret text    DEFAULT NULL,
  p_params         jsonb   DEFAULT '{}'::jsonb
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
  body    text;
  tok     text;
BEGIN
  SELECT value INTO tok FROM synapse.secrets WHERE name = 'admin_token';

  body := json_build_object(
    'name',           p_name,
    'provider',       p_provider,
    'model',          p_model,
    'dimension',      p_dimension,
    'api_key_secret', p_api_key_secret,
    'params',         p_params
  )::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/admin/profile/embedding',
    body    := body,
    headers := json_build_object(
      'Content-Type', 'application/json',
      'X-PG-Synapse-Admin-Token', coalesce(tok, '')
    )::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- synapse.secret_set(name text, value text): store a secret.
CREATE OR REPLACE FUNCTION synapse.secret_set(p_name text, p_value text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
  req_id  bigint;
  result  jsonb;
  body    text;
  tok     text;
BEGIN
  SELECT value INTO tok FROM synapse.secrets WHERE name = 'admin_token';

  body := json_build_object('name', p_name, 'value', p_value)::text;

  SELECT id INTO req_id
  FROM net.http_post(
    url     := '{{SIDECAR_URL}}/v1/admin/secret',
    body    := body,
    headers := json_build_object(
      'Content-Type', 'application/json',
      'X-PG-Synapse-Admin-Token', coalesce(tok, '')
    )::jsonb
  );

  SELECT content::jsonb INTO result
  FROM net.http_collect_response(req_id, async:=false);

  RETURN result;
END;
$$;

-- ---------------------------------------------------------------------------
-- Grant execute on forwarder functions
-- ---------------------------------------------------------------------------

GRANT EXECUTE ON FUNCTION synapse.version()                        TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.version()                        TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.execute(text, text)              TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.execute(text, text)              TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.embed(text, text)                TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.embed(text, text)                TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.execute_async(text, text)        TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.execute_async(text, text)        TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.execution_status(uuid)           TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.execution_status(uuid)           TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.agent_create(text,text,text,text,text[],integer,bigint)
                                                                   TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.llm_profile_set(text,text,text,text,text,jsonb)
                                                                   TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.embedding_profile_set(text,text,text,integer,text,jsonb)
                                                                   TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.secret_set(text, text)          TO synapse_admin;

REVOKE ALL ON FUNCTION synapse.version()                          FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.execute(text, text)               FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.embed(text, text)                 FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.execute_async(text, text)         FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.execution_status(uuid)            FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.agent_create(text,text,text,text,text[],integer,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.llm_profile_set(text,text,text,text,text,jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.embedding_profile_set(text,text,text,integer,text,jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION synapse.secret_set(text, text)            FROM PUBLIC;
