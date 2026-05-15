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

-- synapse.secrets is never directly readable by synapse_user. schema.sql
-- grants table DML only to synapse_admin; synapse_user got SELECT only on
-- executions / messages / traces. Re-assert the prohibition defensively:
-- nothing about secrets is granted to synapse_user here, and the REVOKE
-- above stripped any PUBLIC path. Callers reach secret values exclusively
-- through SECURITY DEFINER functions, never via a direct table read.
REVOKE ALL ON synapse.secrets FROM synapse_user;
