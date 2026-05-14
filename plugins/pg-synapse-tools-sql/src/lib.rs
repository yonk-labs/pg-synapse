//! SQL tool kit for `pg_synapse`.
//!
//! Exposes two agent tools, `sql_query` (read) and `sql_exec` (write), that
//! delegate to a host-provided [`SqlExecutor`] trait. The pgrx host (M7)
//! implements this trait over SPI; the sidecar host (M8) implements it over
//! `sqlx`; tests use the in-memory [`testing::MemorySqlExecutor`].
//!
//! ## Security
//!
//! The executor decides which Postgres role runs the SQL. In the pgrx host
//! the SPI runs as `CURRENT_USER` (the caller of `pg_synapse.execute(...)`),
//! NOT as the `SECURITY DEFINER` role of the wrapping function, so existing
//! role grants gate access. The tools themselves do no privilege analysis,
//! they just forward the SQL + bind params + caller role to the executor.
//!
//! ## Output shape
//!
//! * `sql_query`: `ToolOutput::Json(Value::Array(rows))` where each row is a
//!   JSON object (column name to value).
//! * `sql_exec`: `ToolOutput::Json({ "rows_affected": <u64> })`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Host-supplied executor. Hosts implement this against their SQL surface
/// (SPI inside pgrx, `sqlx::PgPool` inside the sidecar, an in-memory map in
/// tests) and pass an `Arc<dyn SqlExecutor>` to [`SqlToolsPlugin::new`].
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    /// SELECT-style query. `params` is a positional bind list (`$1`, `$2`,
    /// ...). Returns rows as a `Vec` of JSON objects (column name to value).
    async fn query(
        &self,
        sql: &str,
        params: &[Value],
        caller_role: Option<&str>,
    ) -> Result<Vec<Value>, ToolError>;

    /// INSERT / UPDATE / DELETE. Returns the number of rows affected.
    async fn execute(
        &self,
        sql: &str,
        params: &[Value],
        caller_role: Option<&str>,
    ) -> Result<u64, ToolError>;
}

/// Arguments accepted by [`SqlQueryTool`].
#[derive(JsonSchema, Deserialize)]
struct SqlQueryArgs {
    /// SQL SELECT statement with `$1, $2, ...` placeholders.
    query: String,
    /// Positional bind parameters as a JSON array. Pass `[]` if none.
    #[serde(default)]
    params: Vec<Value>,
}

/// `sql_query` tool: runs a parameterized read and returns rows as a JSON
/// array. Backed by a host [`SqlExecutor`] implementation.
pub struct SqlQueryTool {
    /// Host-supplied SQL executor.
    pub executor: Arc<dyn SqlExecutor>,
}

#[async_trait]
impl Tool for SqlQueryTool {
    fn name(&self) -> &str {
        "sql_query"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            let root = schemars::schema_for!(SqlQueryArgs);
            ToolSchema::from_root(root)
        })
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: SqlQueryArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "sql_query".into(),
                reason: e.to_string(),
            })?;
        let rows = self
            .executor
            .query(&args.query, &args.params, ctx.caller_role.as_deref())
            .await?;
        Ok(ToolOutput::Json(Value::Array(rows)))
    }
}

/// Arguments accepted by [`SqlExecTool`].
#[derive(JsonSchema, Deserialize)]
struct SqlExecArgs {
    /// SQL statement (INSERT / UPDATE / DELETE) with `$1, $2, ...` placeholders.
    statement: String,
    /// Positional bind parameters as a JSON array. Pass `[]` if none.
    #[serde(default)]
    params: Vec<Value>,
}

/// `sql_exec` tool: runs a parameterized write and returns the affected row
/// count. Backed by a host [`SqlExecutor`] implementation.
pub struct SqlExecTool {
    /// Host-supplied SQL executor.
    pub executor: Arc<dyn SqlExecutor>,
}

#[async_trait]
impl Tool for SqlExecTool {
    fn name(&self) -> &str {
        "sql_exec"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            let root = schemars::schema_for!(SqlExecArgs);
            ToolSchema::from_root(root)
        })
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: SqlExecArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "sql_exec".into(),
                reason: e.to_string(),
            })?;
        let rows = self
            .executor
            .execute(&args.statement, &args.params, ctx.caller_role.as_deref())
            .await?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "rows_affected": rows }),
        ))
    }
}

/// Plugin that registers both `sql_query` and `sql_exec` against a provided
/// [`SqlExecutor`].
pub struct SqlToolsPlugin {
    executor: Arc<dyn SqlExecutor>,
}

impl SqlToolsPlugin {
    /// Construct a new plugin bound to `executor`.
    pub fn new(executor: Arc<dyn SqlExecutor>) -> Self {
        Self { executor }
    }
}

impl Plugin for SqlToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-sql"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn register(self, registry: &mut Registry) {
        registry.tools.add_arc(
            "sql_query",
            Arc::new(SqlQueryTool {
                executor: self.executor.clone(),
            }),
        );
        registry.tools.add_arc(
            "sql_exec",
            Arc::new(SqlExecTool {
                executor: self.executor.clone(),
            }),
        );
    }
}

/// Test-only `SqlExecutor` implementations.
///
/// `MemorySqlExecutor` is intentionally trivial: it supports only the SQL
/// shapes used by the in-tree integration tests. Production hosts must NOT
/// use it. It is exposed (rather than `#[cfg(test)]`) so downstream crates
/// (the pgrx host, sidecar) can reuse it in their own unit tests without
/// rebuilding it.
pub mod testing {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// In-memory `SqlExecutor`. Stores rows as a list of JSON objects keyed
    /// by table name.
    ///
    /// Supported SQL shapes (parsed by trivial string match):
    ///
    /// * `SELECT * FROM <table>`
    /// * `INSERT INTO <table> VALUES ($1)` (where `$1` is a JSON object row)
    ///
    /// Any other SQL returns `ToolError::Execution`. Test-only; this is
    /// deliberately not a real query planner.
    #[derive(Default)]
    pub struct MemorySqlExecutor {
        tables: Mutex<BTreeMap<String, Vec<Value>>>,
    }

    impl MemorySqlExecutor {
        /// Construct an empty in-memory executor.
        pub fn new() -> Self {
            Self::default()
        }

        /// Insert a JSON-object row into `table`.
        pub fn insert_row(&self, table: &str, row: Value) {
            self.tables
                .lock()
                .unwrap()
                .entry(table.into())
                .or_default()
                .push(row);
        }

        /// Snapshot rows stored under `table` (empty `Vec` if absent).
        pub fn rows(&self, table: &str) -> Vec<Value> {
            self.tables
                .lock()
                .unwrap()
                .get(table)
                .cloned()
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl SqlExecutor for MemorySqlExecutor {
        async fn query(
            &self,
            sql: &str,
            _params: &[Value],
            _caller_role: Option<&str>,
        ) -> Result<Vec<Value>, ToolError> {
            let lower = sql.trim().to_lowercase();
            if let Some(rest) = lower.strip_prefix("select * from ") {
                let table = rest.split_whitespace().next().unwrap_or("").to_string();
                return Ok(self.rows(&table));
            }
            Err(ToolError::Execution {
                name: "sql_query".into(),
                reason: "MemorySqlExecutor only supports `SELECT * FROM <table>` in tests".into(),
            })
        }
        async fn execute(
            &self,
            sql: &str,
            params: &[Value],
            _caller_role: Option<&str>,
        ) -> Result<u64, ToolError> {
            let lower = sql.trim().to_lowercase();
            if let Some(rest) = lower.strip_prefix("insert into ") {
                let table = rest.split_whitespace().next().unwrap_or("").to_string();
                if let Some(row) = params.first() {
                    self.tables
                        .lock()
                        .unwrap()
                        .entry(table)
                        .or_default()
                        .push(row.clone());
                    return Ok(1);
                }
            }
            Err(ToolError::Execution {
                name: "sql_exec".into(),
                reason:
                    "MemorySqlExecutor only supports `INSERT INTO <table> VALUES ($1)` in tests"
                        .into(),
            })
        }
    }

    /// One recorded executor call: `(sql, params, caller_role)`.
    pub type RecordedCall = (String, Vec<Value>, Option<String>);

    /// `SqlExecutor` that records every call it receives, for verifying
    /// `caller_role` propagation and similar passthrough behavior.
    #[derive(Default)]
    pub struct RecordingSqlExecutor {
        /// One entry per `query` call.
        pub query_calls: Mutex<Vec<RecordedCall>>,
        /// One entry per `execute` call.
        pub execute_calls: Mutex<Vec<RecordedCall>>,
    }

    impl RecordingSqlExecutor {
        /// Construct a new recorder.
        pub fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait]
    impl SqlExecutor for RecordingSqlExecutor {
        async fn query(
            &self,
            sql: &str,
            params: &[Value],
            caller_role: Option<&str>,
        ) -> Result<Vec<Value>, ToolError> {
            self.query_calls.lock().unwrap().push((
                sql.to_string(),
                params.to_vec(),
                caller_role.map(str::to_string),
            ));
            Ok(vec![])
        }
        async fn execute(
            &self,
            sql: &str,
            params: &[Value],
            caller_role: Option<&str>,
        ) -> Result<u64, ToolError> {
            self.execute_calls.lock().unwrap().push((
                sql.to_string(),
                params.to_vec(),
                caller_role.map(str::to_string),
            ));
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::MemorySqlExecutor;

    #[test]
    fn plugin_register_inserts_both_tools() {
        let mut reg = Registry::new();
        let exec: Arc<dyn SqlExecutor> = Arc::new(MemorySqlExecutor::new());
        SqlToolsPlugin::new(exec).register(&mut reg);
        let mut names = reg.tools.names();
        names.sort();
        assert_eq!(names, vec!["sql_exec", "sql_query"]);
    }

    #[test]
    fn plugin_metadata_present() {
        let exec: Arc<dyn SqlExecutor> = Arc::new(MemorySqlExecutor::new());
        let p = SqlToolsPlugin::new(exec);
        assert_eq!(p.name(), "pg-synapse-tools-sql");
        assert!(!p.version().is_empty());
    }
}
