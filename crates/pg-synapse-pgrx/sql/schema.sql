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
  -- The role that owns the extension's functions, and therefore the role that
  -- agent SQL executes as.
  --
  -- The SQL surface is SECURITY DEFINER, so agent statements run with the
  -- privileges of whoever owns those functions. Owned by a superuser, that
  -- means an agent tool call can reach the operating system:
  -- `COPY t FROM PROGRAM 'id -un'` is a superuser-only feature and it worked.
  -- A natural-language prompt reaching a shell is not a theoretical concern
  -- for an agent whose job is reading untrusted web pages.
  --
  -- Owning them as a plain role removes the escalation entirely: the same
  -- statement now fails because the role is not a superuser, enforced by
  -- Postgres rather than by our own checking.
  --
  -- NOSUPERUSER and NOBYPASSRLS are stated rather than assumed, because both
  -- are the point.
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'synapse_owner') THEN
    CREATE ROLE synapse_owner NOLOGIN NOSUPERUSER NOCREATEROLE NOBYPASSRLS;
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
  -- The scan of this database, and whether a human has confirmed it. An agent
  -- must not be pointed at a database nobody has looked at: the scan is what a
  -- generated agent reads so it does not invent column names, and the
  -- confirmation is the moment a DBA gets to say no.
  scan_json        JSONB,
  scanned_at       TIMESTAMPTZ,
  reviewed_at      TIMESTAMPTZ,
  reviewed_by      TEXT,
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

-- pgcrypto backs the at-rest encryption of secret values. Available in every
-- standard Postgres distribution as a contrib module.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE IF NOT EXISTS synapse.secrets (
  name        TEXT PRIMARY KEY,
  -- Ciphertext when is_encrypted, plaintext otherwise. Two representations in
  -- one column rather than two columns, because a secret is exactly one value
  -- and a schema that can hold both at once invites them to disagree.
  value       TEXT NOT NULL,
  is_encrypted BOOLEAN NOT NULL DEFAULT false,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The single place a secret is turned back into its value.
--
-- Two callers need this (the profile source when building the kernel, and the
-- remote-database tools when opening a connection) and a second copy of the
-- expression is a second chance to get it wrong. Returns NULL for an unknown
-- name rather than raising, because a missing secret is a condition the caller
-- reports, not an exception.
CREATE OR REPLACE FUNCTION synapse.secret_value(secret_name text)
RETURNS text
LANGUAGE sql
STABLE
AS $$
  SELECT CASE
           WHEN s.is_encrypted
           THEN pgp_sym_decrypt(dearmor(s.value),
                  current_setting('pg_synapse.master_key', true))
           ELSE s.value
         END
  FROM synapse.secrets s
  WHERE s.name = secret_name
$$;

-- Named connections to an external Postgres database, for the
-- remote_query / remote_exec tools (pg-synapse-tools-remotedb). password
-- is never stored here directly: password_secret names a row in
-- synapse.secrets, resolved server-side and never exposed to the model.
CREATE TABLE IF NOT EXISTS synapse.connections (
  name             TEXT PRIMARY KEY,
  host             TEXT NOT NULL,
  port             INT NOT NULL DEFAULT 5432,
  dbname           TEXT NOT NULL,
  "user"           TEXT NOT NULL,
  password_secret  TEXT,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Durable named questions. A question is compiled to SQL once (by an agent or
-- by hand) and executed by Postgres on every later invocation, so no model runs
-- at query time. kind='sql' is deterministic and chartable; kind='agent' is
-- reserved for judgment-shaped questions that must re-run the agent.
-- confirmed_at records that a human approved the compiled SQL: it is the gate
-- that keeps model-authored SQL from running unreviewed.
CREATE TABLE IF NOT EXISTS synapse.questions (
  app          TEXT NOT NULL,
  name         TEXT NOT NULL,
  nl_text      TEXT NOT NULL,
  kind         TEXT NOT NULL DEFAULT 'sql',
  sql_text     TEXT,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  confirmed_at TIMESTAMPTZ,
  PRIMARY KEY (app, name),
  CONSTRAINT questions_kind_check CHECK (kind IN ('sql', 'agent')),
  CONSTRAINT questions_sql_present CHECK (kind <> 'sql' OR sql_text IS NOT NULL)
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
  -- The model that answered this run, resolved from the agent's LLM profile
  -- when the row is written. Auditability needs to answer "which model
  -- produced this", not just "which agent". Resolved at write time rather
  -- than carried through the kernel, so if a profile is repointed mid-run
  -- this records the profile as it stood when the run finished.
  model         TEXT,
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

-- ---------------------------------------------------------------------------
-- Apps: the unit of packaging, addressing, and scheduling.
--
-- An app used to be a naming convention (a schema plus a same-named agent),
-- which meant nothing could enumerate apps, describe one, or attach a schedule
-- or a surface to one. Making it a row is what lets every surface bind to the
-- same thing.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS synapse.apps (
  name         TEXT PRIMARY KEY,
  title        TEXT,
  description  TEXT,
  schema_name  TEXT,
  -- The named connection in synapse.connections this app reaches, when the app
  -- was built against an existing database rather than its own schema. NULL
  -- for greenfield apps.
  connection   TEXT,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- An app may hold more than one agent. Today the builder creates one, but the
-- app is the unit, so the binding is a table rather than a naming rule.
CREATE TABLE IF NOT EXISTS synapse.app_agents (
  app    TEXT NOT NULL REFERENCES synapse.apps(name) ON DELETE CASCADE,
  agent  TEXT NOT NULL,
  PRIMARY KEY (app, agent)
);

-- ---------------------------------------------------------------------------
-- Schedules: what turns an automation into a system.
--
-- An agent that runs once produces a result; an agent that runs on a schedule
-- produces a dataset, and only a dataset can be mined.
--
-- Deliberately an interval plus an explicit next run, not a cron expression.
-- Alignment comes free from choosing the first run: set next_run_at to tomorrow
-- at 09:00 with every_interval '1 day' and it stays at 09:00 forever, with no
-- expression parser and no new dependency.
--
-- ponytail: interval only, so "every Tuesday and Thursday" is not expressible.
-- Upgrade path is a nullable cron_expr column consulted in preference to
-- every_interval, if anyone ever asks for it.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS synapse.schedules (
  schedule_id    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  app            TEXT NOT NULL REFERENCES synapse.apps(name) ON DELETE CASCADE,
  agent          TEXT NOT NULL,
  input          TEXT NOT NULL,
  every_interval INTERVAL NOT NULL,
  next_run_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_run_at    TIMESTAMPTZ,
  enabled        BOOLEAN NOT NULL DEFAULT true,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT schedules_interval_positive CHECK (every_interval > interval '0')
);

CREATE INDEX IF NOT EXISTS schedules_due_idx
  ON synapse.schedules (next_run_at) WHERE enabled;

-- One entry point for any scheduler driver: pg_cron, a sidecar poller, or a
-- systemd timer. Enqueues every due schedule and advances it, returning how
-- many it fired.
--
-- Two behaviours that matter more than the mechanism:
--
--   Overlap. A schedule whose previous run is still queued or running is
--   skipped rather than stacked, so a slow run cannot pile up behind itself.
--
--   Catch up. After downtime a schedule fires ONCE, not once per missed
--   interval. next_run_at advances by whole intervals to the next future
--   occurrence in a single step, which preserves the chosen alignment while
--   refusing to replay a backlog. A catch-up storm on restart would be a
--   self-inflicted denial of service.
CREATE OR REPLACE FUNCTION synapse.tick()
RETURNS integer
LANGUAGE plpgsql
AS $$
DECLARE
  fired integer := 0;
  s     RECORD;
BEGIN
  FOR s IN
    SELECT sc.schedule_id, sc.agent, sc.input, sc.next_run_at, sc.every_interval
    FROM synapse.schedules sc
    WHERE sc.enabled
      AND sc.next_run_at <= now()
      AND NOT EXISTS (
        SELECT 1 FROM synapse.agent_queue q
        WHERE q.agent = sc.agent AND q.status IN ('queued', 'running')
      )
    ORDER BY sc.next_run_at
    FOR UPDATE SKIP LOCKED
  LOOP
    PERFORM synapse.enqueue(s.agent, s.input, 'schedule');

    UPDATE synapse.schedules
       SET next_run_at = s.next_run_at + s.every_interval * (
             floor(
               EXTRACT(epoch FROM (now() - s.next_run_at))
               / EXTRACT(epoch FROM s.every_interval)
             )::int + 1
           ),
           last_run_at = now()
     WHERE schedule_id = s.schedule_id;

    fired := fired + 1;
  END LOOP;

  RETURN fired;
END;
$$;


-- ---------------------------------------------------------------------------
-- Row level provenance.
--
-- synapse.executions answers "what did this agent run". It cannot answer
-- "which run produced this row", which is the question an auditor asks when
-- they are looking at a row they do not trust.
--
-- Deliberately a side table rather than columns on the user's tables. Adding
-- a _synapse_execution_id column to every generated table would pollute a
-- schema the user owns, and could not work at all for brownfield apps, where
-- the customer's database must not be modified.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS synapse.row_provenance (
  execution_id  UUID NOT NULL,
  table_schema  TEXT NOT NULL,
  table_name    TEXT NOT NULL,
  row_pk        TEXT,
  op            TEXT NOT NULL,
  written_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS row_provenance_row_idx
  ON synapse.row_provenance (table_schema, table_name, row_pk);
CREATE INDEX IF NOT EXISTS row_provenance_exec_idx
  ON synapse.row_provenance (execution_id);

-- Stamps a row with the execution that wrote it. Reads the execution id from
-- the session setting the executor publishes; a write made outside an agent
-- run has no id and is not recorded, which is correct: it had no agent
-- provenance to record.
--
-- The primary key is discovered from the catalog rather than assumed to be
-- "id", so this attaches to a table whose key is named anything.
CREATE OR REPLACE FUNCTION synapse.record_provenance()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
  exec_id text := nullif(current_setting('synapse.execution_id', true), '');
  pk_col   text;
  pk_val   text;
  rec      record;
BEGIN
  IF exec_id IS NULL THEN
    RETURN NULL;
  END IF;

  SELECT a.attname INTO pk_col
  FROM pg_index i
  JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
  WHERE i.indrelid = TG_RELID AND i.indisprimary
  LIMIT 1;

  IF pk_col IS NOT NULL THEN
    rec := NEW;
    EXECUTE format('SELECT ($1).%I::text', pk_col) INTO pk_val USING rec;
  END IF;

  INSERT INTO synapse.row_provenance
    (execution_id, table_schema, table_name, row_pk, op)
  VALUES (exec_id::uuid, TG_TABLE_SCHEMA, TG_TABLE_NAME, pk_val, TG_OP);

  RETURN NULL;
END;
$$;

-- Attach provenance recording to one table. Called by the builder for tables
-- it creates, and available to an operator for a table they own.
--
-- SECURITY INVOKER by nature (plain LANGUAGE plpgsql, no DEFINER): creating a
-- trigger is DDL on the caller's table and should need the caller's rights,
-- for the same reason attach_agent_trigger does.
CREATE OR REPLACE FUNCTION synapse.track_provenance(target_schema text, target_table text)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
  EXECUTE format(
    'DROP TRIGGER IF EXISTS synapse_provenance ON %I.%I',
    target_schema, target_table);
  EXECUTE format(
    'CREATE TRIGGER synapse_provenance AFTER INSERT OR UPDATE ON %I.%I '
    'FOR EACH ROW EXECUTE FUNCTION synapse.record_provenance()',
    target_schema, target_table);
END;
$$;

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
GRANT SELECT ON synapse.questions   TO synapse_user;
GRANT SELECT ON synapse.apps        TO synapse_user;
GRANT SELECT ON synapse.app_agents  TO synapse_user;
GRANT SELECT ON synapse.schedules   TO synapse_user;
GRANT SELECT ON synapse.row_provenance TO synapse_user;

-- synapse_owner runs every agent statement, so it needs exactly what the
-- extension itself does and nothing more: its own schema, its own tables, and
-- the ability to create the schemas generated apps live in. Notably absent:
-- superuser, CREATEROLE, and BYPASSRLS.
GRANT USAGE, CREATE ON SCHEMA synapse TO synapse_owner;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA synapse TO synapse_owner;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA synapse TO synapse_owner;
ALTER DEFAULT PRIVILEGES IN SCHEMA synapse
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO synapse_owner;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA synapse TO synapse_admin;
