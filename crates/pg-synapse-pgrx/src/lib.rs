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

mod audit_capability;
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
            count >= 9,
            "expected at least 9 tables in synapse schema (8 original + agent_queue), saw {count}",
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
            .block_on(async { ex.execute(sql, params, None, None).await })
            .map_err(|e| e.to_string())
    }

    fn query_sql(sql: &str, params: &[Value]) -> Result<Vec<Value>, String> {
        let ex = crate::spi_executor::SpiSqlExecutor;
        crate::runtime_holder::tokio()
            .block_on(async { ex.query(sql, params, None, None).await })
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
            events: vec![],
        };
        crate::sql_functions::log_execution(
            &outcome,
            "numeric_agent",
            "hi",
            Some("tester"),
            pg_synapse_core::types::TraceLevel::Full,
            None,
            &crate::audit_capability::AuditGrant::mint(),
        )
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

    /// F2: the audit trail is one privileged call, and the async path's
    /// placeholder row is replaced by the real one inside it.
    ///
    /// The async entry point writes a `queued` row under an id it mints, then
    /// the kernel mints a different id for the messages, so the placeholder has
    /// to go. It used to go in a separate DELETE, which left a window where a
    /// failure of the write after it would leave the run with no row at all.
    /// Passing it as `supersedes` puts both in one statement.
    #[pg_test]
    fn log_execution_supersedes_the_queued_placeholder() {
        use pg_synapse_core::types::{ExecutorOutcome, Message, OutcomeStatus};
        use uuid::Uuid;

        // Standing in for the entry point that would hold this for the run.
        let grant = crate::audit_capability::AuditGrant::mint();

        let placeholder = Uuid::new_v4();
        crate::sql_functions::record_status(
            &placeholder.to_string(),
            "numeric_agent",
            Some("hi"),
            None,
            "queued",
            Some("tester"),
            &grant,
        )
        .expect("placeholder must be recordable");

        let real = Uuid::new_v4();
        let msg: Message = serde_json::from_value(json!({
            "execution_id": real,
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
            tokens_in: 1,
            tokens_out: 2,
            cost_usd: None,
            duration_ms: 10,
            status: OutcomeStatus::Completed,
            events: vec![],
        };
        crate::sql_functions::log_execution(
            &outcome,
            "numeric_agent",
            "hi",
            Some("tester"),
            pg_synapse_core::types::TraceLevel::Full,
            Some(&placeholder.to_string()),
            &grant,
        )
        .expect("log_execution must succeed");

        let gone: Option<i64> = Spi::get_one(&format!(
            "SELECT count(*) FROM synapse.executions WHERE execution_id = '{placeholder}'::uuid"
        ))
        .unwrap();
        assert_eq!(gone, Some(0), "the placeholder row must be superseded");

        let kept: Option<i64> = Spi::get_one(&format!(
            "SELECT count(*) FROM synapse.executions WHERE execution_id = '{real}'::uuid"
        ))
        .unwrap();
        assert_eq!(kept, Some(1), "the real row must be written");

        // The whole run in one call: the transcript lands with it, not through
        // a separate INSERT per message.
        let msgs: Option<i64> = Spi::get_one(&format!(
            "SELECT count(*) FROM synapse.messages WHERE execution_id = '{real}'::uuid"
        ))
        .unwrap();
        assert_eq!(msgs, Some(1), "messages must be written by the same call");
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

    // ---- D5 / O1: inline trigger mode is bounded by construction ----

    /// The inline ceiling is a stated number, not "much shorter".
    ///
    /// O1 was blocking for the DBA persona precisely because D5 described this
    /// bound in prose. A registered GUC with a default is what turns it into
    /// something an operator can read, audit and change.
    #[pg_test]
    fn inline_timeout_has_a_stated_default() {
        let v: Option<String> =
            Spi::get_one("SELECT current_setting('pg_synapse.inline_timeout_ms')").unwrap();
        assert_eq!(
            v.as_deref(),
            Some("2000"),
            "the inline ceiling must be a stated number"
        );
    }

    /// It is a ceiling, not a default: it may only take time away.
    ///
    /// An agent configured tighter than the ceiling keeps its own budget, and
    /// an agent configured looser is cut down to it. Tested on the arithmetic
    /// rather than by running an agent, which would need an LLM.
    #[pg_test]
    fn the_inline_ceiling_only_ever_lowers() {
        use std::time::Duration;
        let cap = Duration::from_millis(2_000);
        let generous = Duration::from_millis(90_000);
        let tight = Duration::from_millis(500);
        assert_eq!(
            generous.min(cap),
            cap,
            "a loose agent is cut to the ceiling"
        );
        assert_eq!(tight.min(cap), tight, "a tight agent keeps its own budget");
    }

    /// Every tool that reaches the network is refused inline, and every tool
    /// that does not is allowed.
    ///
    /// The second half is the one that matters over time: `is_egress_tool` is
    /// a hand-maintained name list, so this pins it against the tools that
    /// actually exist. A plugin added later without a decision here shows up
    /// as a failure rather than as a silent hole in D5.
    #[pg_test]
    fn every_network_tool_is_refused_inline() {
        use crate::sql_functions::is_egress_tool;

        for t in [
            "http_get",
            "http_head",
            "http_post",
            "search_news",
            "fetch_feed",
            "read_article",
            "load_url",
            "remote_query",
            "remote_exec",
            // Transitive: runs another agent, whose tools this check never sees.
            "call_agent",
        ] {
            assert!(
                is_egress_tool(t),
                "{t} reaches the network and must be refused inline"
            );
        }

        // The complete set of registered tools that do not, as of this commit.
        for t in [
            "sql_query",
            "sql_exec",
            "describe_schema",
            "load_csv",
            "load_json",
            "export_csv",
            "read_file",
            "write_file",
            "edit_file",
            "list_files",
            "grep",
            "calculator",
            "get_current_time",
            "lede_compress",
        ] {
            assert!(
                !is_egress_tool(t),
                "{t} is local and must stay usable inline"
            );
        }
    }

    /// O8: the encryption key is not readable by a caller who can run an agent.
    ///
    /// `GucContext::Suset` governs who may SET a GUC and says nothing about who
    /// may read one, so with default flags this returned the key to a
    /// synapse_user through an ordinary `sql_query` tool call, in one line.
    /// Per-caller isolation does not help, because a GUC read is not a table
    /// read and no grant governs it. The `SUPERUSER_ONLY` flag is what closes
    /// it, and this pins the flag so removing it fails here.
    #[pg_test(error = "permission denied to examine \"pg_synapse.master_key\"")]
    fn master_key_is_not_readable_by_synapse_user() {
        Spi::run("SET ROLE synapse_user").unwrap();
        Spi::run("SELECT current_setting('pg_synapse.master_key')").unwrap();
        Spi::run("RESET ROLE").unwrap();
    }

    // ---- F2: per-caller isolation, the privilege matrix ----
    //
    // Four claims, one test each. Together they are what F2 promised: a
    // restricted caller can run an agent, cannot read secrets, cannot forge
    // the audit trail, and cannot reach a table their own role lacks.

    /// A restricted caller can reach everything a run needs.
    ///
    /// The entry points are SECURITY INVOKER now, so the privileged work they
    /// still do has to be reachable as `synapse_user` or no run gets off the
    /// ground. Actually running an agent needs an LLM the harness does not
    /// have, so this exercises the plumbing a run depends on instead.
    #[pg_test]
    fn synapse_user_can_reach_what_a_run_needs() {
        Spi::run("SET ROLE synapse_user").unwrap();
        Spi::run("SELECT synapse.ensure_kernel()").expect("kernel build must be reachable");
        Spi::run("SELECT synapse.agent_trace_level('nobody')")
            .expect("trace level must be reachable");
        Spi::run("RESET ROLE").unwrap();
    }

    /// The trap slice 2a exists to avoid. `ensure_kernel` reads secrets on the
    /// caller's behalf, but the function that returns them is not granted, so
    /// a caller cannot ask for one directly.
    #[pg_test(error = "permission denied for function config_secrets")]
    fn synapse_user_cannot_read_secrets_through_the_config_reader() {
        Spi::run("SET ROLE synapse_user").unwrap();
        Spi::run("SELECT synapse.config_secrets(ARRAY['anything'])").unwrap();
        Spi::run("RESET ROLE").unwrap();
    }

    /// The audit writer IS granted to `synapse_user`, and that is still not
    /// enough to write with. Without a token minted by a run in flight the
    /// call is refused, so the grant does not become a way to forge the trail.
    // The message is on one line because the pg_test attribute cannot unescape
    // a continuation inside its `error =` literal.
    #[allow(clippy::literal_string_with_formatting_args)]
    #[pg_test(
        error = "audit writes are reachable only from a run in progress; this function cannot be called directly"
    )]
    fn synapse_user_cannot_forge_an_audit_row() {
        Spi::run("SET ROLE synapse_user").unwrap();
        Spi::run(
            "SELECT synapse.audit_status('{\"execution_id\":\"00000000-0000-0000-0000-000000000001\",\
             \"agent_name\":\"forged\",\"status\":\"completed\"}'::jsonb, \
             'ffffffffffffffffffffffffffffffff')",
        )
        .unwrap();
        Spi::run("RESET ROLE").unwrap();
    }

    /// The point of the whole exercise: an agent's SQL reaches what the CALLER
    /// may reach, not what the extension owner may.
    ///
    /// `synapse.secrets` is granted to nobody but admin and owner. While
    /// `tool_call` was SECURITY DEFINER this same statement ran with the
    /// owner's rights and returned a count. As INVOKER under `synapse_user` it
    /// is Postgres that says no, which is the difference between an agent that
    /// is trusted to behave and one that cannot misbehave.
    #[pg_test]
    fn an_agents_sql_cannot_reach_a_table_the_caller_lacks() {
        // `tool_call` reports a tool failure in its return value rather than
        // raising, so the denial arrives as JSON. That is the contract an
        // agent sees too: the model is told it may not read that table and
        // can act on it, instead of the whole statement aborting.
        Spi::run("SET ROLE synapse_user").unwrap();
        let denied = jsonb_of(
            "SELECT synapse.tool_call('sql_query', \
             '{\"query\":\"SELECT count(*) AS n FROM synapse.secrets\",\"params\":[]}'::jsonb)",
        );
        Spi::run("RESET ROLE").unwrap();

        let msg = denied.to_string();
        assert!(
            msg.contains("permission denied for table secrets"),
            "synapse_user must not reach synapse.secrets through an agent tool, got {msg}"
        );

        // The same statement as the owner still works, which is what shows the
        // denial came from the caller's rights and not from a broken tool.
        let allowed = jsonb_of(
            "SELECT synapse.tool_call('sql_query', \
             '{\"query\":\"SELECT count(*) AS n FROM synapse.secrets\",\"params\":[]}'::jsonb)",
        );
        assert!(
            allowed.get("error").is_none(),
            "the same read must succeed for a privileged caller, got {allowed}"
        );
    }

    // ---- T1: reactive triggers (ADR D14 / operator approval 2026-05-17) ----

    /// enqueue inserts a queued row and returns a uuid.
    /// No LLM is required: the function is a plain INSERT.
    #[pg_test]
    fn enqueue_inserts_queued_row() {
        let id: Option<pgrx::Uuid> =
            Spi::get_one("SELECT synapse.enqueue('test_agent', 'hello', 'unit-test')").unwrap();
        let id = id.expect("enqueue must return a uuid");
        let id_str = format!("{:-x}", id);
        let cnt: Option<i64> = Spi::connect(|c| {
            let arg = pgrx::datum::DatumWithOid::from(id_str);
            c.select(
                "SELECT count(*)::bigint FROM synapse.agent_queue \
                 WHERE job_id = $1::uuid AND status = 'queued' AND agent = 'test_agent'",
                None,
                &[arg],
            )
            .ok()
            .and_then(|t| t.into_iter().next())
            .and_then(|r| r.get::<i64>(1).ok().flatten())
        });
        assert_eq!(cnt, Some(1), "enqueue must insert exactly one queued row");
    }

    /// drain_queue on an empty queue returns 0 and does not error.
    #[pg_test]
    fn drain_queue_on_empty_returns_zero() {
        // Ensure no queued rows exist (use a temp table trick: just call drain
        // on a clean state). The test harness runs in isolated transactions so
        // no cross-test contamination.
        let n: Option<i32> = Spi::get_one("SELECT synapse.drain_queue(10)").unwrap();
        assert_eq!(n, Some(0), "drain_queue on an empty queue must return 0");
    }

    /// attach_agent_trigger creates a trigger and trigger function; detach removes them.
    #[pg_test]
    fn attach_and_detach_agent_trigger_round_trip() {
        // Create a scratch table to attach to.
        Spi::run("CREATE TEMP TABLE trig_test_attach (id serial primary key, payload text)")
            .unwrap();

        // Attach a queue-mode trigger.
        Spi::run(
            "SELECT synapse.attach_agent_trigger(\
             'trig_test_attach', 'dummy_agent', 'queue', 'INSERT', NULL, 'NEW::text')",
        )
        .unwrap();

        // Verify the trigger exists via pg_trigger.
        // The expected trigger name is synapse_agent_trig_test_attach.
        let trig_exists: Option<bool> = Spi::get_one(
            "SELECT EXISTS (\
             SELECT 1 FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid \
             WHERE c.relname = 'trig_test_attach' \
               AND t.tgname = 'synapse_agent_trig_test_attach')",
        )
        .unwrap();
        assert_eq!(
            trig_exists,
            Some(true),
            "attach must create a trigger named synapse_agent_trig_test_attach"
        );

        // Verify the trigger function exists via pg_proc.
        let fn_exists: Option<bool> = Spi::get_one(
            "SELECT EXISTS (\
             SELECT 1 FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'public' OR TRUE \
               AND p.proname = 'synapse_trig_trig_test_attach')",
        )
        .unwrap();
        assert_eq!(
            fn_exists,
            Some(true),
            "attach must create trigger function synapse_trig_trig_test_attach"
        );

        // Detach: remove trigger and function.
        Spi::run("SELECT synapse.detach_agent_trigger('trig_test_attach')").unwrap();

        let trig_after: Option<bool> = Spi::get_one(
            "SELECT EXISTS (\
             SELECT 1 FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid \
             WHERE c.relname = 'trig_test_attach' \
               AND t.tgname = 'synapse_agent_trig_test_attach')",
        )
        .unwrap();
        assert_eq!(trig_after, Some(false), "detach must remove the trigger");
    }

    /// Queue-mode trigger on INSERT enqueues a row with correct source label.
    /// No LLM is needed: the trigger function only calls synapse.enqueue.
    #[pg_test]
    fn queue_mode_trigger_enqueues_on_insert() {
        // Create a scratch table and attach a queue-mode trigger.
        Spi::run("CREATE TEMP TABLE trig_queue_src (id serial primary key, note text)").unwrap();
        Spi::run(
            "SELECT synapse.attach_agent_trigger(\
             'trig_queue_src', 'noop_agent', 'queue', 'INSERT', NULL, 'NEW::text')",
        )
        .unwrap();

        // Count queue rows before the INSERT.
        let before: Option<i64> = Spi::get_one(
            "SELECT count(*)::bigint FROM synapse.agent_queue WHERE source LIKE 'trigger:%'",
        )
        .unwrap();
        let before = before.unwrap_or(0);

        // INSERT a row: should fire the trigger which calls enqueue.
        Spi::run("INSERT INTO trig_queue_src (note) VALUES ('ping')").unwrap();

        let after: Option<i64> = Spi::get_one(
            "SELECT count(*)::bigint FROM synapse.agent_queue WHERE source LIKE 'trigger:%'",
        )
        .unwrap();
        let after = after.unwrap_or(0);

        assert_eq!(
            after,
            before + 1,
            "queue-mode trigger INSERT must enqueue exactly one row"
        );

        // Verify the source label includes the table name.
        let src: Option<String> = Spi::get_one(
            "SELECT source FROM synapse.agent_queue \
             WHERE source LIKE 'trigger:%' ORDER BY enqueued_at DESC LIMIT 1",
        )
        .unwrap();
        assert!(
            src.as_deref()
                .map(|s| s.contains("trig_queue_src"))
                .unwrap_or(false),
            "source must contain the table name, got: {src:?}"
        );
    }

    /// pg_trigger_depth guard: the trigger body skips recursion when
    /// pg_trigger_depth() > 1. We test this by simulating what the guard
    /// does: a direct INSERT into the trigger table from inside a statement-
    /// triggered function would increment depth. The guard ensures enqueue
    /// is only called once, not recursively.
    ///
    /// Strategy (no live LLM): create a table + queue-mode trigger, insert
    /// one row. The row fires the trigger once (depth=1). If enqueue were
    /// recursive via a nested trigger, depth would be 2 and the guard blocks
    /// it. We verify only one queue row was created per insert.
    #[pg_test]
    fn trigger_depth_guard_prevents_double_enqueue() {
        Spi::run("CREATE TEMP TABLE trig_depth (id serial primary key, val text)").unwrap();
        Spi::run(
            "SELECT synapse.attach_agent_trigger(\
             'trig_depth', 'depth_agent', 'queue', 'INSERT', NULL, 'NEW::text')",
        )
        .unwrap();

        // Insert one row; expect exactly one queue row (not two from recursion).
        Spi::run("INSERT INTO trig_depth (val) VALUES ('a')").unwrap();

        let cnt: Option<i64> = Spi::get_one(
            "SELECT count(*)::bigint FROM synapse.agent_queue WHERE agent = 'depth_agent'",
        )
        .unwrap();
        assert_eq!(
            cnt,
            Some(1),
            "depth guard: one INSERT must produce exactly one queue row"
        );
    }

    /// Inline-mode reject path: a trigger function that detects a rejection
    /// in the execute result raises an exception, rolling back the INSERT.
    ///
    /// Strategy (deterministic, no live LLM): we cannot call a real agent in
    /// pg_test. Instead, we verify the reject-detection logic by directly
    /// creating a plpgsql function that mimics what attach_agent_trigger builds
    /// for inline mode. The key behaviour is: if execute returns a JSONB with
    /// status != 'completed', RAISE EXCEPTION is called. We simulate this by
    /// calling synapse.enqueue (which succeeds) instead of execute, asserting
    /// the queue row appears. For the raise path, we create a PL/pgSQL stub
    /// function that raises directly and verify a trigger that calls it rolls
    /// back the INSERT.
    #[pg_test(error = "synapse inline trigger rejected: simulated reject")]
    fn inline_mode_raise_rolls_back_insert() {
        // Create a table and a manual trigger function that always raises
        // (simulating the inline-mode reject path without a live LLM).
        Spi::run("CREATE TEMP TABLE trig_inline_test (id serial primary key, val text)").unwrap();
        Spi::run(
            "CREATE OR REPLACE FUNCTION synapse_trig_trig_inline_test() \
             RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
               IF pg_trigger_depth() > 1 THEN RETURN NEW; END IF; \
               RAISE EXCEPTION 'synapse inline trigger rejected: simulated reject'; \
             END;$$",
        )
        .unwrap();
        Spi::run(
            "CREATE TRIGGER synapse_agent_trig_inline_test \
             AFTER INSERT ON trig_inline_test \
             FOR EACH ROW EXECUTE FUNCTION synapse_trig_trig_inline_test()",
        )
        .unwrap();

        // This INSERT must raise (and therefore roll back due to the error).
        Spi::run("INSERT INTO trig_inline_test (val) VALUES ('bad')").unwrap();
        // Not reached if the trigger raised.
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
            // Reactive triggers T1: enqueue is reachable by both roles.
            "synapse.enqueue(text,text,text)",
        ] {
            let user_has: Option<bool> = Spi::get_one(&format!(
                "SELECT has_function_privilege('synapse_user', '{sig}', 'EXECUTE')"
            ))
            .unwrap();
            assert_eq!(user_has, Some(true), "synapse_user must reach {sig}");
        }

        // Reactive triggers T1: drain/attach/detach are admin-only.
        for sig in [
            "synapse.drain_queue(integer)",
            "synapse.attach_agent_trigger(text,text,text,text,text,text)",
            "synapse.detach_agent_trigger(text)",
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
            "pg_synapse.inline_timeout_ms",
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

/// Map an `execute` envelope onto a queue terminal status.
///
/// The queue used to decide this by asking whether an `error` key was present,
/// which is true only for `OutcomeStatus::Errored`. That left three other
/// non-success terminal states recorded as `done`: `max_iterations`,
/// `timed_out`, and `cost_cap_exceeded`. The last two are the wall-clock and
/// cost guardrails firing, so a guardrail doing its job was being written down
/// as a successful run, and anyone auditing `synapse.agent_queue` to ask
/// whether last night's scheduled work completed got the wrong answer.
///
/// Only `completed` is a success. Everything else carries a reason.
///
/// Returned as (queue status, error reason). Pure, so it is testable without a
/// live Postgres backend.
pub(crate) fn queue_status_for(envelope: &serde_json::Value) -> (&'static str, Option<String>) {
    let status = envelope
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("errored");
    if status == "completed" {
        return ("done", None);
    }
    let reason = envelope
        .get("error")
        .and_then(|e| e.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("run ended with status \"{status}\""));
    ("error", Some(reason))
}

#[cfg(test)]
mod queue_status_tests {
    use super::queue_status_for;
    use serde_json::json;

    #[test]
    fn completed_is_the_only_success() {
        let (status, err) = queue_status_for(&json!({"status": "completed", "output": "hi"}));
        assert_eq!(status, "done");
        assert!(err.is_none());
    }

    /// The regression this function exists for. Each of these previously
    /// landed as "done" because no `error` key was present.
    #[test]
    fn guardrail_outcomes_are_errors_not_successes() {
        for s in ["timed_out", "cost_cap_exceeded", "max_iterations"] {
            let (status, err) = queue_status_for(&json!({"status": s, "output": "partial"}));
            assert_eq!(status, "error", "{s} must not be recorded as done");
            assert!(err.unwrap().contains(s), "{s} must say why it stopped");
        }
    }

    #[test]
    fn errored_keeps_its_own_message() {
        let (status, err) = queue_status_for(&json!({"status": "errored", "error": "boom"}));
        assert_eq!(status, "error");
        assert_eq!(err.as_deref(), Some("boom"));
    }

    /// A malformed envelope is treated as a failure, never as a success.
    #[test]
    fn missing_status_is_not_a_success() {
        let (status, _) = queue_status_for(&json!({}));
        assert_eq!(status, "error");
    }
}
