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

    // ---- B5: stringified-param coercion (unknown-OID binding) ----
    //
    // LLMs routinely emit numeric ids as JSON strings, e.g. "3" instead of 3.
    // Before B5, json_to_datum bound JSON strings as TEXT (TEXTOID), which
    // caused Postgres to raise "operator does not exist: bigint = text" for
    // predicates like `WHERE id = $1` when id is a bigint column.
    // The fix binds JSON strings as UNKNOWNOID (OID 705) so Postgres applies
    // the same context-driven coercion it uses for untyped SQL literals.

    /// B5: a stringified integer id coerces to bigint via UNKNOWNOID binding.
    /// This is the exact failure shape from the gpt-5-mini s2_triage benchmark
    /// run: `WHERE id = $1` with params: ["3"].
    #[pg_test]
    fn bind_stringified_int_id_coerces() {
        Spi::run("CREATE TEMP TABLE bq_b5a (id bigint primary key, label text)").unwrap();
        Spi::run("INSERT INTO bq_b5a VALUES (3, 'three')").unwrap();

        let rows = query_sql("SELECT label FROM bq_b5a WHERE id = $1", &[json!("3")])
            .expect("stringified-int predicate must succeed, not raise bigint=text");
        assert_eq!(rows.len(), 1, "must match exactly one row");
        assert_eq!(rows[0]["label"], json!("three"));
    }

    /// B5 non-regression: a numeric JSON number still binds as INT8 and works.
    #[pg_test]
    fn bind_numeric_id_still_works() {
        Spi::run("CREATE TEMP TABLE bq_b5b (id bigint primary key, label text)").unwrap();
        Spi::run("INSERT INTO bq_b5b VALUES (3, 'three')").unwrap();

        let rows = query_sql("SELECT label FROM bq_b5b WHERE id = $1", &[json!(3)])
            .expect("numeric-int predicate must still work");
        assert_eq!(rows.len(), 1, "must match exactly one row");
        assert_eq!(rows[0]["label"], json!("three"));
    }

    /// B5: the exact s2_triage failure shape: UPDATE with mixed string params
    /// where the second param is a stringified bigint id.
    #[pg_test]
    fn bind_update_with_stringified_id() {
        Spi::run("CREATE TEMP TABLE bq_b5c (id bigint primary key, label text)").unwrap();
        Spi::run("INSERT INTO bq_b5c VALUES (3, 'three')").unwrap();

        let n = exec_sql(
            "UPDATE bq_b5c SET label = $1 WHERE id = $2",
            &[json!("updated"), json!("3")],
        )
        .expect("UPDATE with stringified id must succeed");
        assert_eq!(n, 1, "must update exactly one row");

        let stored: Option<String> = Spi::get_one("SELECT label FROM bq_b5c WHERE id = 3").unwrap();
        assert_eq!(
            stored.as_deref(),
            Some("updated"),
            "row must reflect the new label"
        );
    }

    /// B5 non-regression: text column predicate with a plain string still works.
    #[pg_test]
    fn bind_text_predicate_still_works() {
        Spi::run("CREATE TEMP TABLE bq_b5d (id bigint primary key, label text)").unwrap();
        Spi::run("INSERT INTO bq_b5d VALUES (3, 'three')").unwrap();

        let rows = query_sql("SELECT id FROM bq_b5d WHERE label = $1", &[json!("three")])
            .expect("text-column predicate must still work after unknown-OID change");
        assert_eq!(rows.len(), 1, "must match exactly one row");
        assert_eq!(rows[0]["id"], json!(3));
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

    // ---- N2.1: NUMERIC cost roundtrip + cost-cap surface ----

    use pg_synapse_core::runtime::ProfileSource;

    /// N2.1: `cost_cap_usd` (NUMERIC) survives the round trip into
    /// `AgentRow.cost_cap_usd` (Option<f64>) through `SpiProfileSource`.
    #[pg_test]
    fn agent_cost_cap_roundtrips() {
        Spi::run(
            "SELECT synapse.agent_create('cap_agent', 'p', 'conversation', 'x', ARRAY[]::text[], 5, 30000)",
        )
        .unwrap();
        Spi::run("UPDATE synapse.agents SET cost_cap_usd = 1.250000 WHERE name = 'cap_agent'")
            .unwrap();

        let src = crate::spi_executor::SpiProfileSource;
        let agents = crate::runtime_holder::tokio()
            .block_on(async { src.agents().await })
            .expect("agents() must succeed");
        let a = agents
            .iter()
            .find(|a| a.name == "cap_agent")
            .expect("cap_agent must be present");
        assert_eq!(a.cost_cap_usd, Some(1.25), "cost cap must round-trip");
    }

    /// N2.1: a fractional `cost_usd` written through `log_execution` is stored
    /// as NUMERIC and preserved to 6 decimals (not lossily stringified).
    #[pg_test]
    fn execution_logs_numeric_cost() {
        use pg_synapse_core::types::{ExecutorOutcome, Message, OutcomeStatus};
        use uuid::Uuid;

        let eid = Uuid::new_v4();
        // Build the Message via serde_json so the test does not need to name
        // the chrono timestamp type (chrono is a core dep, not a pgrx dep).
        let msg: Message = serde_json::from_value(json!({
            "execution_id": eid,
            "seq": 0,
            "role": "assistant",
            "content": "done",
            "tool_call_id": null,
            "tool_name": null,
            "tool_input": null,
            "tool_output": null,
            "timestamp": "1970-01-01T00:00:00Z",
        }))
        .expect("Message must deserialize");
        let outcome = ExecutorOutcome {
            output: "done".into(),
            messages: vec![msg],
            tool_calls: vec![],
            tokens_in: 3,
            tokens_out: 4,
            cost_usd: Some(0.123456),
            duration_ms: 10,
            status: OutcomeStatus::Completed,
        };
        crate::sql_functions::log_execution(&outcome, "numeric_agent", "hi", Some("tester"))
            .expect("log_execution must succeed");

        let arg: pgrx::datum::DatumWithOid<'_> = pgrx::datum::DatumWithOid::from(eid.to_string());
        let cost: Option<f64> = Spi::connect(|c| {
            let t = c
                .select(
                    "SELECT cost_usd::float8 FROM synapse.executions WHERE execution_id = $1::uuid",
                    None,
                    &[arg],
                )
                .unwrap();
            t.into_iter()
                .next()
                .and_then(|r| r.get::<f64>(1).ok().flatten())
        });
        let cost = cost.expect("cost_usd row must exist");
        assert!(
            (cost - 0.123456).abs() < 1e-9,
            "NUMERIC cost must be preserved to 6 decimals, got {cost}"
        );
    }

    // ---- N2.2: remaining SQL functions ----

    use serde_json::Value as JsonValue;

    fn jsonb_of(sql: &str) -> JsonValue {
        let v: Option<pgrx::JsonB> = Spi::get_one(sql).unwrap();
        v.expect("function returned a JSONB value").0
    }

    #[pg_test]
    fn agent_list_returns_created_agents() {
        Spi::run(
            "SELECT synapse.agent_create('list_a', 'p', 'conversation', 'x', ARRAY['sql_query']::text[], 5, 30000)",
        )
        .unwrap();
        let v = jsonb_of("SELECT synapse.agent_list()");
        let arr = v.as_array().expect("agent_list returns an array");
        let found = arr.iter().any(|a| a["name"] == "list_a");
        assert!(found, "agent_list must include list_a: {v}");
    }

    #[pg_test]
    fn tool_register_and_tool_list_roundtrip() {
        Spi::run(
            "SELECT synapse.tool_register('my_tool', 'a test tool', '{\"type\":\"object\"}'::jsonb, 'manual', '{}'::jsonb)",
        )
        .unwrap();
        let v = jsonb_of("SELECT synapse.tool_list()");
        let arr = v.as_array().expect("tool_list returns an array");
        let row = arr
            .iter()
            .find(|t| t["name"] == "my_tool")
            .expect("tool_list must include my_tool");
        assert_eq!(row["description"], "a test tool");
        assert_eq!(row["kind"], "manual");
    }

    #[pg_test]
    fn llm_profile_drop_removes_row() {
        Spi::run(
            "SELECT synapse.llm_profile_set('dropme', 'openai', 'm', NULL, NULL, '{}'::jsonb)",
        )
        .unwrap();
        let before: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.llm_profiles WHERE name='dropme'")
                .unwrap();
        assert_eq!(before, Some(1));
        Spi::run("SELECT synapse.llm_profile_drop('dropme')").unwrap();
        let after: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.llm_profiles WHERE name='dropme'")
                .unwrap();
        assert_eq!(after, Some(0), "llm_profile_drop must remove the row");
    }

    #[pg_test]
    fn secret_drop_removes_row() {
        Spi::run("SELECT synapse.secret_set('sdrop', 'val')").unwrap();
        let before: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.secrets WHERE name='sdrop'")
                .unwrap();
        assert_eq!(before, Some(1));
        Spi::run("SELECT synapse.secret_drop('sdrop')").unwrap();
        let after: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.secrets WHERE name='sdrop'")
                .unwrap();
        assert_eq!(after, Some(0), "secret_drop must remove the row");
    }

    #[pg_test]
    fn execution_status_not_found_for_random_uuid() {
        let v = jsonb_of("SELECT synapse.execution_status(gen_random_uuid())");
        assert_eq!(v["status"], "not_found");
    }

    #[pg_test]
    fn execute_async_returns_uuid_and_logs_row() {
        // No live LLM in the harness: the kernel build/run will error, but the
        // contract holds: a uuid is returned and a row exists with that id.
        let id: Option<pgrx::Uuid> =
            Spi::get_one("SELECT synapse.execute_async('no_such_agent', 'hello')").unwrap();
        let id = id.expect("execute_async must return a uuid");
        let id_str = format!("{:-x}", id);
        let arg: pgrx::datum::DatumWithOid<'_> = pgrx::datum::DatumWithOid::from(id_str);
        let cnt: Option<i64> = Spi::connect(|c| {
            let t = c
                .select(
                    "SELECT count(*)::bigint FROM synapse.executions WHERE execution_id = $1::uuid",
                    None,
                    &[arg],
                )
                .unwrap();
            t.into_iter()
                .next()
                .and_then(|r| r.get::<i64>(1).ok().flatten())
        });
        assert_eq!(cnt, Some(1), "execute_async must leave exactly one row");
    }

    #[pg_test]
    fn tool_call_invokes_registered_tool() {
        // sql_query needs no LLM. tool_call resolves it through the kernel and
        // returns the rows as JSONB.
        let v = jsonb_of(
            "SELECT synapse.tool_call('sql_query', '{\"query\":\"SELECT 1 AS x\",\"params\":[]}'::jsonb)",
        );
        let arr = v.as_array().expect("sql_query returns a JSON array");
        assert_eq!(arr.len(), 1, "one row expected: {v}");
        assert_eq!(arr[0]["x"], 1);
    }

    /// N2.2 grant matrix: drops / register are admin-only; list / status /
    /// tool_call reachable by synapse_user. Asserted via privilege
    /// introspection (no error raised, stays in one transaction).
    #[pg_test]
    fn new_functions_grant_matrix() {
        // Admin-only: synapse_user must NOT have EXECUTE.
        for sig in [
            "synapse.tool_register(text,text,jsonb,text,jsonb)",
            "synapse.llm_profile_drop(text)",
            "synapse.embedding_profile_drop(text)",
            "synapse.secret_drop(text)",
        ] {
            let user_has: Option<bool> = Spi::get_one(&format!(
                "SELECT has_function_privilege('synapse_user', '{sig}', 'EXECUTE')"
            ))
            .unwrap();
            assert_eq!(user_has, Some(false), "synapse_user must NOT reach {sig}");
            let admin_has: Option<bool> = Spi::get_one(&format!(
                "SELECT has_function_privilege('synapse_admin', '{sig}', 'EXECUTE')"
            ))
            .unwrap();
            assert_eq!(admin_has, Some(true), "synapse_admin must reach {sig}");
        }

        // Both roles: list / status / tool_call / execute_async.
        for sig in [
            "synapse.agent_list()",
            "synapse.tool_list()",
            "synapse.tool_call(text,jsonb)",
            "synapse.execute_async(text,text)",
            "synapse.execution_status(uuid)",
        ] {
            let user_has: Option<bool> = Spi::get_one(&format!(
                "SELECT has_function_privilege('synapse_user', '{sig}', 'EXECUTE')"
            ))
            .unwrap();
            assert_eq!(user_has, Some(true), "synapse_user must reach {sig}");
        }
    }

    // ---- N2.3: full GUC set + fallback resolution ----

    #[pg_test]
    fn all_gucs_registered() {
        let names = [
            "pg_synapse.disable_builtin_sql_tools",
            "pg_synapse.default_llm_profile_main",
            "pg_synapse.default_llm_profile_small",
            "pg_synapse.default_llm_profile_judge",
            "pg_synapse.default_embedding_profile",
            "pg_synapse.default_timeout_ms",
            "pg_synapse.default_timeout_seconds",
            "pg_synapse.default_max_iterations",
            "pg_synapse.default_cost_cap_usd",
            "pg_synapse.trace_enabled",
            "pg_synapse.sidecar_url",
            "pg_synapse.master_key",
            "pg_synapse.compression_threshold_tokens",
            "pg_synapse.default_executor",
        ];
        for n in names {
            let present: Option<bool> = Spi::get_one(&format!(
                "SELECT EXISTS (SELECT 1 FROM pg_settings WHERE name = '{n}')"
            ))
            .unwrap();
            assert_eq!(present, Some(true), "GUC {n} must be registered");
        }
    }

    #[pg_test]
    fn guc_fallback_fills_missing_llm_profile() {
        Spi::run("SET pg_synapse.default_llm_profile_main = 'gucprofile'").unwrap();
        // Insert an agent with NULL llm_profile_main directly.
        Spi::run(
            "INSERT INTO synapse.agents (name, system_prompt, executor_name) VALUES ('guc_a', 'p', 'conversation')",
        )
        .unwrap();
        let src = crate::spi_executor::SpiProfileSource;
        let agents = crate::runtime_holder::tokio()
            .block_on(async { src.agents().await })
            .unwrap();
        let a = agents.iter().find(|a| a.name == "guc_a").expect("guc_a");
        assert_eq!(
            a.llm_profile_main.as_deref(),
            Some("gucprofile"),
            "NULL llm_profile_main must resolve from the GUC"
        );
        Spi::run("RESET pg_synapse.default_llm_profile_main").unwrap();
    }

    #[pg_test]
    fn guc_fallback_timeout_and_max_iterations() {
        Spi::run("SET pg_synapse.default_timeout_ms = 12345").unwrap();
        Spi::run("SET pg_synapse.default_max_iterations = 7").unwrap();
        Spi::run(
            "INSERT INTO synapse.agents (name, system_prompt, executor_name, max_iterations, timeout_ms) VALUES ('guc_t', 'p', 'conversation', 0, 0)",
        )
        .unwrap();
        let src = crate::spi_executor::SpiProfileSource;
        let agents = crate::runtime_holder::tokio()
            .block_on(async { src.agents().await })
            .unwrap();
        let a = agents.iter().find(|a| a.name == "guc_t").expect("guc_t");
        assert_eq!(a.timeout_ms, 12345, "timeout must resolve from GUC");
        assert_eq!(a.max_iterations, 7, "max_iterations must resolve from GUC");
        Spi::run("RESET pg_synapse.default_timeout_ms").unwrap();
        Spi::run("RESET pg_synapse.default_max_iterations").unwrap();
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
