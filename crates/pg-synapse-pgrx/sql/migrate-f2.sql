-- F2 slice 2 migration for an already-installed database.
-- schema.sql runs only at CREATE EXTENSION, so a live DB needs this by hand.
BEGIN;

-- 1. new SQL helper
CREATE OR REPLACE FUNCTION synapse.agent_trace_level(p_agent text)
RETURNS text LANGUAGE sql SECURITY DEFINER STABLE AS $$
  SELECT trace_level FROM synapse.agents WHERE name = p_agent
$$;

-- 2. new C functions
CREATE  FUNCTION synapse."ensure_kernel"() RETURNS void
STRICT SECURITY DEFINER
LANGUAGE c /* Rust */
AS '$libdir/pg_synapse_pgrx', 'ensure_kernel_wrapper';
CREATE  FUNCTION synapse."audit_run"(
	"payload" jsonb, /* JsonB */
	"token" TEXT /* & str */
) RETURNS void
STRICT SECURITY DEFINER
LANGUAGE c /* Rust */
AS '$libdir/pg_synapse_pgrx', 'audit_run_wrapper';
CREATE  FUNCTION synapse."audit_status"(
	"payload" jsonb, /* JsonB */
	"token" TEXT /* & str */
) RETURNS void
STRICT SECURITY DEFINER
LANGUAGE c /* Rust */
AS '$libdir/pg_synapse_pgrx', 'audit_status_wrapper';

-- 3. F2: the entry points run as their caller now
ALTER FUNCTION synapse.execute(text, text) SECURITY INVOKER;
ALTER FUNCTION synapse.execute_async(text, text) SECURITY INVOKER;
ALTER FUNCTION synapse.tool_call(text, jsonb) SECURITY INVOKER;

-- 3b. Pre-existing drift, found while verifying the above rather than caused
-- by it. attach_agent_trigger and detach_agent_trigger were made SECURITY
-- INVOKER in the source some time ago, deliberately: attaching a trigger is
-- DDL against the caller's own table, so it should need the caller's own
-- privileges, and a table owner may instrument their table while nobody may
-- instrument a table they do not own. A database installed before that change
-- still has them as DEFINER, which is the opposite.
--
-- The general lesson, worth more than these two lines: hot-swapping the .so
-- updates the Rust and nothing else. Every CREATE FUNCTION on a live database
-- is frozen at CREATE EXTENSION time, so security mode, argument types and
-- new functions all drift silently. Diff pg_proc.prosecdef against
-- `cargo pgrx schema` after any hot swap.
ALTER FUNCTION synapse.attach_agent_trigger(text, text, text, text, text, text) SECURITY INVOKER;
ALTER FUNCTION synapse.detach_agent_trigger(text) SECURITY INVOKER;

-- 4. Ownership. A SECURITY DEFINER function owned by the installing
-- superuser runs as one, which is what the ownership loop in grants.sql
-- exists to prevent. That loop runs only at CREATE EXTENSION, so anything
-- added to a live database by hand lands owned by whoever ran the DDL.
-- record_run and record_status were created as postgres in the previous
-- slice and were superuser-definer until this line.
DO $own$ DECLARE r record; BEGIN
  FOR r IN SELECT p.oid::regprocedure AS f FROM pg_proc p
           JOIN pg_namespace n ON n.oid = p.pronamespace
           WHERE n.nspname = 'synapse' AND pg_get_userbyid(p.proowner) <> 'synapse_owner'
  LOOP EXECUTE format('ALTER FUNCTION %s OWNER TO synapse_owner', r.f); END LOOP;
END $own$;

-- 5. What an INVOKER entry point still needs. config_secrets is
-- deliberately absent: granting it is SELECT any_secret_you_like.
GRANT EXECUTE ON FUNCTION synapse.ensure_kernel() TO synapse_user, synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.agent_trace_level(text) TO synapse_user, synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.audit_run(jsonb, text) TO synapse_user, synapse_admin;
GRANT EXECUTE ON FUNCTION synapse.audit_status(jsonb, text) TO synapse_user, synapse_admin;

COMMIT;
