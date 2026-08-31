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
-- This constrains what an agent can do. It does NOT make an agent run as the
-- role that invoked it: Postgres forbids SET ROLE inside a SECURITY DEFINER
-- function, so per-caller isolation needs these entry points to become
-- SECURITY INVOKER first. Tracked as F2 in spec/pg-one/SPEC.md.
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
