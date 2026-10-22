-- pg_synapse_pgrx grants: the G9 auth boundary.
--
-- Embedded via `extension_sql_file!(..., finalize)` in src/lib.rs so this runs
-- AFTER pgrx has emitted every `CREATE FUNCTION synapse.*`. (The bootstrap
-- schema.sql runs first; function creation runs in the middle; this runs
-- last.) Ordering matters: GRANT/REVOKE on functions can only succeed once
-- the functions exist.
--
-- Model: callers never touch synapse.secrets or any synapse function via
-- PUBLIC. Admin / write functions require synapse_admin. Run / read
-- functions are granted to synapse_user AND synapse_admin. Every function is
-- SECURITY DEFINER (set in the Rust #[pg_extern(security_definer)] attrs), so
-- the function body runs with the extension owner's rights while the GRANT
-- gates who may invoke it.

-- Strip the default PUBLIC privileges. PUBLIC must reach nothing here.
REVOKE ALL ON SCHEMA synapse FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA synapse FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA synapse FROM PUBLIC;

-- Schema usage: both roles need USAGE to resolve synapse.* names. (schema.sql
-- already granted these on bootstrap; re-assert in case the bootstrap GRANT
-- was rolled back or the schema pre-existed.)
GRANT USAGE ON SCHEMA synapse TO synapse_admin;
GRANT USAGE ON SCHEMA synapse TO synapse_user;

-- Admin / write surface: synapse_admin only.
GRANT EXECUTE ON FUNCTION synapse.agent_create(text, text, text, text, text[], integer, bigint) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.agent_drop(text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.llm_profile_set(text, text, text, text, text, jsonb) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.embedding_profile_set(text, text, text, integer, text, jsonb) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.secret_set(text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.rebuild_kernel() TO synapse_admin;
-- v0.1.1 N2.2 admin / write surface (registers, drops): synapse_admin only.
GRANT EXECUTE ON FUNCTION synapse.tool_register(text, text, jsonb, text, jsonb) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.llm_profile_drop(text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.embedding_profile_drop(text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.secret_drop(text) TO synapse_admin;

-- Run / read surface: synapse_user AND synapse_admin.
GRANT EXECUTE ON FUNCTION synapse.execute(text, text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.execute(text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.embed(text, text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.embed(text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.version() TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.version() TO synapse_admin;
-- v0.1.1 N2.2 run / read / list / status / tool_call surface: both roles.
GRANT EXECUTE ON FUNCTION synapse.agent_list() TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.agent_list() TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.tool_list() TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.tool_list() TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.tool_call(text, jsonb) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.tool_call(text, jsonb) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.execute_async(text, text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.execute_async(text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.execution_status(uuid) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.execution_status(uuid) TO synapse_admin;

-- Reactive triggers surface (T1, ADR D14 / operator approval 2026-05-17).
-- enqueue: both roles (writers need to enqueue rows from trigger context).
GRANT EXECUTE ON FUNCTION synapse.enqueue(text, text, text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.enqueue(text, text, text) TO synapse_admin;
-- drain_queue: admin only (runs agent execution, potentially expensive).
GRANT EXECUTE ON FUNCTION synapse.drain_queue(integer) TO synapse_admin;
-- attach/detach: admin only (creates DDL objects in the database).
GRANT EXECUTE ON FUNCTION synapse.attach_agent_trigger(text, text, text, text, text, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.detach_agent_trigger(text) TO synapse_admin;

-- ---------------------------------------------------------------------------
-- F2: what an entry point running as its caller still needs.
--
-- `execute`, `execute_async` and `tool_call` are SECURITY INVOKER, so the
-- agent's SQL runs with the caller's own privileges and Postgres enforces
-- what that role may reach. The three things those functions do that are not
-- the agent's SQL still need the owner's rights, and each is granted on its
-- own terms rather than by trust.
--
-- ensure_kernel: builds the process-local kernel cache, which reads agents,
-- both profile tables and the secrets those profiles name. Safe to hand out
-- because of its SHAPE, not its contents: no argument to steer it, no value
-- returned. Granting the `synapse.config_*` functions it calls instead would
-- NOT be safe: config_secrets(names text[]) in a caller's hands is
-- `SELECT any_secret_you_like`, verified on a live database. They stay
-- owner-only and are absent from this file deliberately.
GRANT EXECUTE ON FUNCTION synapse.ensure_kernel() TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.ensure_kernel() TO synapse_admin;

-- agent_trace_level: one agent's verbosity setting, resolved on every run to
-- decide what to persist. synapse.agents is not readable by synapse_user and
-- should not become so. Argument names an agent, answer is one of five words.
GRANT EXECUTE ON FUNCTION synapse.agent_trace_level(text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.agent_trace_level(text) TO synapse_admin;

-- audit_run / audit_status: the audit trail. These are the ones where the
-- grant alone would have been a hole, because the payload is the forgery: a
-- role that may write the audit trail may lie in it. So the grant is not the
-- authorisation. Each refuses any call that cannot present a capability token
-- minted by an entry point on this backend and retired when the run ends, so a
-- caller invoking them directly is refused by the function they were granted.
-- The unguarded synapse.record_run / synapse.record_status underneath stay
-- owner-only. See crates/pg-synapse-pgrx/src/audit_capability.rs.
GRANT EXECUTE ON FUNCTION synapse.audit_run(jsonb, text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.audit_run(jsonb, text) TO synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.audit_status(jsonb, text) TO synapse_user;
GRANT EXECUTE ON FUNCTION synapse.audit_status(jsonb, text) TO synapse_admin;

-- synapse.secrets is never directly readable by synapse_user. schema.sql
-- grants table DML only to synapse_admin; synapse_user got SELECT only on
-- executions / messages / traces. Re-assert the prohibition defensively:
-- nothing about secrets is granted to synapse_user here, and the REVOKE
-- above stripped any PUBLIC path. Callers reach secret values exclusively
-- through SECURITY DEFINER functions, never via a direct table read.
REVOKE ALL ON synapse.secrets FROM synapse_user;

-- ---------------------------------------------------------------------------
-- Ownership: agent SQL must not run as a superuser.
--
-- Every function above is SECURITY DEFINER, so an agent's statements execute
-- with the privileges of whoever owns them. Owned by the installing superuser,
-- that means a tool call can reach the operating system: verified on
-- 2026-08-31, `COPY t FROM PROGRAM 'id -un'` through synapse.tool_call
-- returned `postgres`. For an agent whose job is reading untrusted web pages,
-- a natural-language prompt reaching a shell is not a hypothetical.
--
-- Reassigning ownership to a plain role closes it without changing any Rust.
-- The same statement now fails with "permission denied to COPY to or from an
-- external program" because the role genuinely lacks the privilege, enforced
-- by Postgres rather than by us remembering to check.
--
-- Looped over the catalog rather than listed, so a function added later is
-- covered automatically instead of being quietly left running as superuser.
--
-- This constrains what an agent can do for the functions that are still
-- SECURITY DEFINER. The three entry points are no longer among them: F2 is
-- done, and an agent invoked through them runs as the role that invoked it.
--
-- Postgres forbids SET ROLE inside a SECURITY DEFINER function, verified in
-- all three forms (SET ROLE, SET LOCAL ROLE, and a SET role = clause on the
-- function), so there was no way to drop privilege part way through a call.
-- The entry points had to become SECURITY INVOKER outright, which is why the
-- three grants above exist.
-- What the constrained owner still needs.
--
-- attach_agent_trigger creates trigger objects on the caller's tables, and the
-- builder creates a schema per generated app, so the owner needs CREATE where
-- those live. `public` is the default because that is where an unconfigured
-- database keeps its tables.
--
-- An operator running this for real should REVOKE this and grant only the
-- schemas agents are meant to touch. It is a default, not a recommendation,
-- and it is deliberately narrower than what was here before: this role is not
-- a superuser, so no grant on a schema can restore COPY ... FROM PROGRAM or
-- pg_read_file.
GRANT USAGE, CREATE ON SCHEMA public TO synapse_owner;

-- Apps built before this ownership change are owned by the installing
-- superuser, so the agent that maintains them (now synapse_owner) cannot ALTER
-- their tables: "must be owner of table". Anything built from here on is
-- created by synapse_owner and owned correctly on the way in, so this is a
-- one-time adoption of what already exists rather than ongoing policy.
--
-- Scoped to schemas registered in synapse.apps. A table the user made and
-- never told pg-one about is left alone, and giving an agent ownership of it
-- stays an explicit operator decision.
DO $adopt_existing_apps$
DECLARE
  r record;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='synapse' AND tablename='apps') THEN
    RETURN;
  END IF;
  FOR r IN SELECT n.nspname FROM pg_namespace n
           JOIN synapse.apps a ON a.schema_name = n.nspname
  LOOP
    EXECUTE format('ALTER SCHEMA %I OWNER TO synapse_owner', r.nspname);
  END LOOP;
  FOR r IN SELECT schemaname, tablename FROM pg_tables
           WHERE schemaname IN (SELECT schema_name FROM synapse.apps WHERE schema_name IS NOT NULL)
  LOOP
    EXECUTE format('ALTER TABLE %I.%I OWNER TO synapse_owner', r.schemaname, r.tablename);
  END LOOP;
  FOR r IN SELECT sequence_schema, sequence_name FROM information_schema.sequences
           WHERE sequence_schema IN (SELECT schema_name FROM synapse.apps WHERE schema_name IS NOT NULL)
  LOOP
    EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO synapse_owner', r.sequence_schema, r.sequence_name);
  END LOOP;
END
$adopt_existing_apps$;

DO $harden_ownership$
DECLARE
  fn record;
BEGIN
  FOR fn IN
    SELECT p.oid::regprocedure AS sig
    FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'synapse'
  LOOP
    EXECUTE format('ALTER FUNCTION %s OWNER TO synapse_owner', fn.sig);
  END LOOP;
END
$harden_ownership$;
