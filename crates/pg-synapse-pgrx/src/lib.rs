//! `pg_synapse_pgrx` is the Postgres extension host for `pg_synapse`.
//!
//! It wires the kernel ([`pg_synapse_core::Runtime`]) into Postgres via pgrx,
//! exposing a small SQL surface under the `synapse` schema:
//!
//! * `synapse.execute(agent_name text, input text) -> jsonb`
//! * `synapse.agent_create(...)`, `agent_drop(name)`
//! * `synapse.llm_profile_set(...)`
//! * `synapse.secret_set(name, value)`
//! * `synapse.version() -> text`
//!
//! GUCs live under the `pg_synapse.*` namespace (custom GUC prefixes are
//! allowed even when the matching schema name would be reserved).
//!
//! A single `current_thread` tokio runtime is built at backend startup
//! (`_PG_init`) and reused for every call. The kernel is built lazily on first
//! `execute()` (reading agent / profile / secret rows from `synapse.*` via
//! SPI) and rebuilt whenever an admin function mutates configuration.

// `deny` rather than `forbid`: the SAVEPOINT-per-tool-call isolation (N1.2)
// requires one tightly-scoped `unsafe` block to drive Postgres internal
// subtransactions (the C-level mechanism PL/pgSQL's `BEGIN ... EXCEPTION`
// uses). SQL `SAVEPOINT` statements are rejected inside a SECURITY DEFINER
// function, so the SQL-only approach cannot work in production. The single
// allowed `unsafe` site is `spi_executor::with_tool_subtransaction`.
#![deny(unsafe_code)]
#![allow(non_snake_case)]
#![warn(missing_docs)]

use pgrx::prelude::*;

pgrx::pg_module_magic!(name, version);

mod runtime_holder;
mod schema_guc;
mod spi_executor;
mod sql_functions;

pub use runtime_holder::{kernel_handle, rebuild_kernel};

/// pgrx-invoked at extension load. Registers GUCs and builds the shared tokio
/// runtime. The kernel itself is built lazily on first `execute()`.
#[pg_guard]
extern "C-unwind" fn _PG_init() {
    schema_guc::register_gucs();
    runtime_holder::initialize_tokio_runtime();
}

// Schema bootstrap: creates the synapse schema, tables, and roles when the
// extension is installed (`CREATE EXTENSION pg_synapse_pgrx`).
extension_sql_file!("../sql/schema.sql", name = "synapse_schema", bootstrap);

// The G9 auth boundary. `finalize` makes pgrx emit this block LAST, after
// every `CREATE FUNCTION synapse.*`, so the GRANT/REVOKE statements can see
// the functions they reference. See sql/grants.sql for the policy rationale.
extension_sql_file!("../sql/grants.sql", name = "synapse_grants", finalize);

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn version_returns_a_string() {
        let v: Option<String> = Spi::get_one("SELECT synapse.version()").unwrap();
        let v = v.expect("version() must return a value");
        assert!(!v.is_empty(), "version string must be non-empty");
    }

    #[pg_test]
    fn schema_tables_exist() {
        let count: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM pg_tables WHERE schemaname = 'synapse'")
                .unwrap();
        let count = count.unwrap_or(0);
        assert!(
            count >= 8,
            "expected at least 8 tables in synapse schema, saw {count}",
        );
    }

    #[pg_test]
    fn roles_exist() {
        let admin: Option<bool> =
            Spi::get_one("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='synapse_admin')")
                .unwrap();
        let user_: Option<bool> =
            Spi::get_one("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='synapse_user')")
                .unwrap();
        assert_eq!(admin, Some(true));
        assert_eq!(user_, Some(true));
    }

    #[pg_test]
    fn agent_and_profile_writes_persist() {
        Spi::run(
            "SELECT synapse.llm_profile_set('test-llm', 'openai', 'gpt-test', NULL, NULL, '{}'::jsonb)",
        )
        .unwrap();
        Spi::run(
            "SELECT synapse.agent_create('echo_agent', 'You are helpful.', 'conversation', 'test-llm', ARRAY[]::text[], 5, 30000)",
        )
        .unwrap();

        let agent_count: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.agents WHERE name='echo_agent'")
                .unwrap();
        assert_eq!(agent_count, Some(1));

        let profile_count: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.llm_profiles WHERE name='test-llm'")
                .unwrap();
        assert_eq!(profile_count, Some(1));
    }

    // ---- N1.1: SECURITY DEFINER + role-scoped GRANTs ----

    /// As `synapse_user`, a direct read of `synapse.secrets` must be denied:
    /// callers reach secret values only through SECURITY DEFINER functions.
    /// This runs in its own isolated test session, so the privilege error
    /// aborting the transaction is expected and harmless.
    #[pg_test(error = "permission denied for table secrets")]
    fn secrets_table_not_readable_by_synapse_user() {
        Spi::run("SET ROLE synapse_user").unwrap();
        // This statement must raise insufficient_privilege.
        let _ = Spi::get_one::<i64>("SELECT count(*)::bigint FROM synapse.secrets");
        // Not reached; the SELECT above aborts the transaction. RESET ROLE
        // here would never run, which is fine for an isolated error test.
        Spi::run("RESET ROLE").unwrap();
    }

    /// As `synapse_user`, an admin function (`secret_set`) must be denied by
    /// the GRANT boundary even though it is SECURITY DEFINER. SECURITY
    /// DEFINER governs *whose rights the body runs with*, not *who may call*;
    /// the missing EXECUTE grant blocks the call.
    #[pg_test(error = "permission denied for function secret_set")]
    fn admin_function_denied_to_synapse_user() {
        Spi::run("SET ROLE synapse_user").unwrap();
        Spi::run("SELECT synapse.secret_set('x', 'y')").unwrap();
        Spi::run("RESET ROLE").unwrap();
    }

    /// As `synapse_user`, a run/read function (`version`) must succeed: it is
    /// granted to synapse_user. Using `version()` avoids needing a live LLM.
    #[pg_test]
    fn execute_allowed_for_synapse_user() {
        Spi::run("SET ROLE synapse_user").unwrap();
        let v: Option<String> = Spi::get_one("SELECT synapse.version()").unwrap();
        Spi::run("RESET ROLE").unwrap();
        assert!(
            v.is_some_and(|s| !s.is_empty()),
            "synapse_user must be able to call synapse.version()"
        );
    }

    /// The GRANT/REVOKE state itself, asserted via Postgres privilege
    /// introspection (no error raised, so this stays in one transaction).
    /// Proves: synapse_user has no privilege on synapse.secrets; admin
    /// functions are admin-only; run functions are open to both roles;
    /// PUBLIC reaches nothing.
    #[pg_test]
    fn grant_matrix_is_correct() {
        let secrets_user: Option<bool> =
            Spi::get_one("SELECT has_table_privilege('synapse_user', 'synapse.secrets', 'SELECT')")
                .unwrap();
        assert_eq!(
            secrets_user,
            Some(false),
            "synapse_user must NOT have SELECT on synapse.secrets"
        );

        let secret_set_user: Option<bool> = Spi::get_one(
            "SELECT has_function_privilege('synapse_user', 'synapse.secret_set(text,text)', 'EXECUTE')",
        )
        .unwrap();
        assert_eq!(
            secret_set_user,
            Some(false),
            "synapse_user must NOT have EXECUTE on synapse.secret_set"
        );

        let secret_set_admin: Option<bool> = Spi::get_one(
            "SELECT has_function_privilege('synapse_admin', 'synapse.secret_set(text,text)', 'EXECUTE')",
        )
        .unwrap();
        assert_eq!(
            secret_set_admin,
            Some(true),
            "synapse_admin must have EXECUTE on synapse.secret_set"
        );

        let version_user: Option<bool> = Spi::get_one(
            "SELECT has_function_privilege('synapse_user', 'synapse.version()', 'EXECUTE')",
        )
        .unwrap();
        assert_eq!(
            version_user,
            Some(true),
            "synapse_user must have EXECUTE on synapse.version"
        );

        let execute_user: Option<bool> = Spi::get_one(
            "SELECT has_function_privilege('synapse_user', 'synapse.execute(text,text)', 'EXECUTE')",
        )
        .unwrap();
        assert_eq!(
            execute_user,
            Some(true),
            "synapse_user must have EXECUTE on synapse.execute"
        );

        // PUBLIC must reach nothing: no ACL entry granting EXECUTE to the
        // pseudo-role PUBLIC (grantee OID 0) on secret_set. aclexplode turns
        // the proacl array into rows; grantee = 0 is PUBLIC.
        let public_grants: Option<i64> = Spi::get_one(
            "SELECT count(*)::bigint \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             CROSS JOIN LATERAL aclexplode(p.proacl) a \
             WHERE n.nspname = 'synapse' AND p.proname = 'secret_set' \
               AND a.grantee = 0 AND a.privilege_type = 'EXECUTE'",
        )
        .unwrap();
        assert_eq!(
            public_grants,
            Some(0),
            "PUBLIC must NOT have EXECUTE on synapse.secret_set"
        );
    }

    // ---- N1.2 + N1.3: SAVEPOINT isolation + positional bind params ----
    //
    // These drive `SpiSqlExecutor` directly through the shared current-thread
    // tokio runtime (no LLM in the harness). The executor's async methods do
    // no real I/O suspension, so `block_on` resolves them inline on the
    // backend thread, exactly as `synapse.execute` does in production.

    use pg_synapse_tools_sql::SqlExecutor;
    use serde_json::{Value, json};

    fn exec_sql(sql: &str, params: &[Value]) -> Result<u64, String> {
        let ex = crate::spi_executor::SpiSqlExecutor;
        crate::runtime_holder::tokio()
            .block_on(async { ex.execute(sql, params, None).await })
            .map_err(|e| e.to_string())
    }

    fn query_sql(sql: &str, params: &[Value]) -> Result<Vec<Value>, String> {
        let ex = crate::spi_executor::SpiSqlExecutor;
        crate::runtime_holder::tokio()
            .block_on(async { ex.query(sql, params, None).await })
            .map_err(|e| e.to_string())
    }

    /// N1.2: a failing tool call rolls back ONLY its own writes. Row A
    /// (a prior successful call) survives; row B (the failing call's partial
    /// write) does not.
    #[pg_test]
    fn failed_sql_exec_rolls_back_only_itself() {
        Spi::run("CREATE TEMP TABLE n12 (id int primary key, label text)").unwrap();

        // Call 1: succeeds, inserts row A.
        let n = exec_sql("INSERT INTO n12 (id, label) VALUES (1, 'A')", &[])
            .expect("first insert must succeed");
        assert_eq!(n, 1);

        // Call 2: inserts row B then violates the PK in the same statement
        // batch, so the whole call errors and must roll back to its
        // savepoint, discarding row B.
        let err = exec_sql(
            "INSERT INTO n12 (id, label) VALUES (2, 'B'); INSERT INTO n12 (id, label) VALUES (1, 'dup')",
            &[],
        )
        .expect_err("second call must fail on the duplicate key");
        assert!(
            err.contains("duplicate key") || err.contains("unique"),
            "unexpected error: {err}"
        );

        // Row A survived; row B was rolled back with the failing call.
        let a: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM n12 WHERE id = 1 AND label = 'A'").unwrap();
        assert_eq!(a, Some(1), "row A must survive the later failed call");
        let b: Option<i64> = Spi::get_one("SELECT count(*)::bigint FROM n12 WHERE id = 2").unwrap();
        assert_eq!(b, Some(0), "row B must have been rolled back");
    }

    /// N1.2: two successful calls both commit, in order.
    #[pg_test]
    fn successful_calls_commit_in_order() {
        Spi::run("CREATE TEMP TABLE n12b (id int primary key)").unwrap();
        assert_eq!(exec_sql("INSERT INTO n12b VALUES (1)", &[]).unwrap(), 1);
        assert_eq!(exec_sql("INSERT INTO n12b VALUES (2)", &[]).unwrap(), 1);
        let c: Option<i64> = Spi::get_one("SELECT count(*)::bigint FROM n12b").unwrap();
        assert_eq!(c, Some(2), "both successful inserts must persist");
    }

    /// N1.3: `sql_exec` binds positional params instead of rejecting them.
    #[pg_test]
    fn sql_exec_binds_positional_params() {
        Spi::run("CREATE TEMP TABLE n13 (a text, b bigint)").unwrap();
        let n = exec_sql(
            "INSERT INTO n13 (a, b) VALUES ($1, $2)",
            &[json!("hello"), json!(42)],
        )
        .expect("parameterized insert must succeed");
        assert_eq!(n, 1);

        let a: Option<String> = Spi::get_one("SELECT a FROM n13").unwrap();
        let b: Option<i64> = Spi::get_one("SELECT b FROM n13").unwrap();
        assert_eq!(a.as_deref(), Some("hello"));
        assert_eq!(b, Some(42));
    }

    /// N1.3: `sql_query` binds positional params through the to_jsonb wrapper.
    #[pg_test]
    fn sql_query_binds_positional_params() {
        Spi::run("CREATE TEMP TABLE n13q (a text, n int)").unwrap();
        Spi::run("INSERT INTO n13q VALUES ('hello', 1), ('world', 2)").unwrap();

        let rows = query_sql("SELECT a, n FROM n13q WHERE a = $1", &[json!("hello")])
            .expect("parameterized query must succeed");
        assert_eq!(rows.len(), 1, "exactly one row should match $1 = 'hello'");
        assert_eq!(rows[0]["a"], json!("hello"));
        assert_eq!(rows[0]["n"], json!(1));
    }

    /// N1.3: a value that looks like a SQL injection payload is treated as
    /// pure data when bound positionally. The temp table survives and holds
    /// exactly the literal string.
    #[pg_test]
    fn injection_value_is_treated_as_data() {
        Spi::run("CREATE TEMP TABLE n13i (a text)").unwrap();
        let payload = "x'); DROP TABLE n13i; --";

        let n = exec_sql("INSERT INTO n13i (a) VALUES ($1)", &[json!(payload)])
            .expect("insert with injection-shaped value must succeed");
        assert_eq!(n, 1);

        // Table still exists (DROP did not execute).
        let exists: Option<bool> = Spi::get_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'n13i')",
        )
        .unwrap();
        assert_eq!(exists, Some(true), "n13i must still exist; DROP was data");

        // It contains exactly the literal payload, once.
        let stored: Option<String> = Spi::get_one("SELECT a FROM n13i").unwrap();
        assert_eq!(stored.as_deref(), Some(payload));
        let cnt: Option<i64> = Spi::get_one("SELECT count(*)::bigint FROM n13i").unwrap();
        assert_eq!(cnt, Some(1));
    }
}

/// pgrx test framework hook.
#[cfg(test)]
pub mod pg_test {
    /// Per-test setup (none required for this extension).
    pub fn setup(_options: Vec<&str>) {}

    /// No postgresql.conf options required.
    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
