//! `remote_query` / `remote_exec`: tools that run SQL against a named
//! external Postgres database, resolved from `synapse.connections` (and,
//! for the password, `synapse.secrets`).
//!
//! This is the sidecar's own trick, reused as a tool instead of a whole
//! separate service: `pg-synapse-sidecar` already connects to a remote
//! Postgres it cannot extend and drives `sql_query` / `sql_exec` against it
//! via a sqlx-backed [`pg_synapse_sidecar::db::SqlxSqlExecutor`]. These
//! tools open that same kind of connection on demand, per named connection,
//! from inside the agent's own process, so no second service has to run.
//!
//! ## Wiring
//!
//! The host constructs [`RemoteDbToolsPlugin`] with the *local* database's
//! [`SqlExecutor`] (the same one backing `sql_query` / `sql_exec`), used
//! only to look up a named connection's credentials. The tools then open a
//! fresh, short-lived connection to the *remote* database for the actual
//! query, and never expose the resolved password to the model: `password`
//! comes out of `synapse.secrets` server-side, never through a tool-call
//! argument or the trace.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use pg_synapse_sidecar::db::SqlxSqlExecutor;
use pg_synapse_tools_sql::SqlExecutor;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A resolved row from `synapse.connections`, with its secret (if any)
/// already looked up.
#[derive(Debug)]
struct ConnectionInfo {
    host: String,
    port: i32,
    dbname: String,
    user: String,
    password: Option<String>,
}

fn get_str(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Look up a named connection via the *local* executor. `tool_name` is only
/// used to attribute a clear error back to whichever tool called this.
async fn resolve_connection(
    local: &dyn SqlExecutor,
    name: &str,
    tool_name: &str,
) -> Result<ConnectionInfo, ToolError> {
    let rows = local
        .query(
            "SELECT host, port, dbname, \"user\", password_secret \
             FROM synapse.connections WHERE name = $1",
            &[Value::String(name.to_owned())],
            None,
        )
        .await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| ToolError::InvalidInput {
            name: tool_name.into(),
            reason: format!(
                "no connection named \"{name}\" (add one from pg-one's Connections page first, \
             or check synapse.connections for the exact name)"
            ),
        })?;

    let host = get_str(&row, "host").ok_or_else(|| ToolError::Execution {
        name: tool_name.into(),
        reason: format!("connection \"{name}\" is missing a host"),
    })?;
    let dbname = get_str(&row, "dbname").ok_or_else(|| ToolError::Execution {
        name: tool_name.into(),
        reason: format!("connection \"{name}\" is missing a dbname"),
    })?;
    let user = get_str(&row, "user").ok_or_else(|| ToolError::Execution {
        name: tool_name.into(),
        reason: format!("connection \"{name}\" is missing a user"),
    })?;
    let port = row.get("port").and_then(Value::as_i64).unwrap_or(5432) as i32;

    let password = match get_str(&row, "password_secret") {
        Some(secret_name) => {
            let srows = local
                .query(
                    "SELECT value FROM synapse.secrets WHERE name = $1",
                    &[Value::String(secret_name.clone())],
                    None,
                )
                .await?;
            let value = srows.first().and_then(|r| get_str(r, "value"));
            if value.is_none() {
                return Err(ToolError::Execution {
                    name: tool_name.into(),
                    reason: format!(
                        "connection \"{name}\" references secret \"{secret_name}\", which does not exist"
                    ),
                });
            }
            value
        }
        None => None,
    };

    Ok(ConnectionInfo {
        host,
        port,
        dbname,
        user,
        password,
    })
}

/// Open a fresh, single-connection pool to the remote database described by
/// `info`, wrapped in the same [`SqlExecutor`] the sidecar uses. Not pooled
/// across calls: a demo-scale tool that runs occasionally does not need
/// connection reuse, and a fresh connection per call means a rotated
/// remote password takes effect on the very next tool call.
///
/// `read_only` opens the session with `default_transaction_read_only=on`, so
/// the remote Postgres itself rejects any write. `remote_query` describes
/// itself to the model as read-only; without this it was not, and an INSERT
/// passed to it succeeded while returning an innocuous empty result set.
/// Enforcing it server-side beats inspecting the SQL: no parser to outwit,
/// and CTEs, functions, and triggers are covered for free.
async fn open_remote(
    info: &ConnectionInfo,
    tool_name: &str,
    read_only: bool,
) -> Result<SqlxSqlExecutor, ToolError> {
    let mut opts = PgConnectOptions::new()
        .host(&info.host)
        .port(info.port as u16)
        .username(&info.user)
        .database(&info.dbname)
        .disable_statement_logging();
    if let Some(pw) = &info.password {
        opts = opts.password(pw);
    }
    if read_only {
        opts = opts.options([("default_transaction_read_only", "on")]);
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| ToolError::Execution {
            name: tool_name.into(),
            reason: format!("could not connect to {}:{}: {e}", info.host, info.port),
        })?;
    Ok(SqlxSqlExecutor::new(pool))
}

/// Arguments shared by `remote_query` and `remote_exec`.
#[derive(JsonSchema, Deserialize)]
struct RemoteArgs {
    /// Name of a row in synapse.connections (set up via pg-one's
    /// Connections page, or synapse.connections directly).
    #[serde(alias = "connection_name", alias = "conn", alias = "database")]
    connection: String,
    /// SQL statement to run against that remote database, with `$1, $2,
    /// ...` placeholders.
    #[serde(alias = "sql", alias = "statement", alias = "query_text")]
    query: String,
    /// Positional bind parameters as a JSON array. Pass `[]` if none.
    #[serde(default)]
    params: Vec<Value>,
}

/// `remote_query`: read-only SELECT against a named external database.
pub struct RemoteQueryTool {
    /// Executor for the LOCAL database, used only to resolve the named
    /// connection (synapse.connections / synapse.secrets).
    pub local: Arc<dyn SqlExecutor>,
}

#[async_trait]
impl Tool for RemoteQueryTool {
    fn name(&self) -> &str {
        "remote_query"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| ToolSchema::from_root(schemars::schema_for!(RemoteArgs)))
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: RemoteArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "remote_query".into(),
                reason: e.to_string(),
            })?;
        let info =
            resolve_connection(self.local.as_ref(), &args.connection, "remote_query").await?;
        let remote = open_remote(&info, "remote_query", true).await?;
        let rows = remote
            .query(&args.query, &args.params, ctx.caller_role.as_deref())
            .await?;
        Ok(ToolOutput::Json(Value::Array(rows)))
    }
}

/// `remote_exec`: INSERT / UPDATE / DELETE against a named external
/// database. Returns the affected row count.
pub struct RemoteExecTool {
    /// Executor for the LOCAL database, used only to resolve the named
    /// connection (synapse.connections / synapse.secrets).
    pub local: Arc<dyn SqlExecutor>,
}

#[async_trait]
impl Tool for RemoteExecTool {
    fn name(&self) -> &str {
        "remote_exec"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| ToolSchema::from_root(schemars::schema_for!(RemoteArgs)))
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: RemoteArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "remote_exec".into(),
                reason: e.to_string(),
            })?;
        let info = resolve_connection(self.local.as_ref(), &args.connection, "remote_exec").await?;
        let remote = open_remote(&info, "remote_exec", false).await?;
        let rows_affected = remote
            .execute(&args.query, &args.params, ctx.caller_role.as_deref())
            .await?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "rows_affected": rows_affected }),
        ))
    }
}

/// Registers `remote_query` and `remote_exec` against the local executor
/// used to resolve named connections.
pub struct RemoteDbToolsPlugin {
    local: Arc<dyn SqlExecutor>,
}

impl RemoteDbToolsPlugin {
    /// Construct a new plugin. `local` is the SAME executor already backing
    /// `sql_query` / `sql_exec` for the local database.
    pub fn new(local: Arc<dyn SqlExecutor>) -> Self {
        Self { local }
    }
}

impl Plugin for RemoteDbToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-remotedb"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn register(self, registry: &mut Registry) {
        registry.tools.add_arc(
            "remote_query",
            Arc::new(RemoteQueryTool {
                local: self.local.clone(),
            }),
        );
        registry.tools.add_arc(
            "remote_exec",
            Arc::new(RemoteExecTool {
                local: self.local.clone(),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Minimal fake `SqlExecutor` backing exactly the two lookups
    /// `resolve_connection` makes: connections and secrets, both keyed by
    /// name. Good enough to test the resolution/error paths without a real
    /// database; `pg_synapse_tools_sql::testing::MemorySqlExecutor` only
    /// supports `SELECT * FROM <table>`, not the `WHERE name = $1` shape
    /// used here.
    #[derive(Default)]
    struct FakeLocal {
        connections: Mutex<HashMap<String, Value>>,
        secrets: Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl SqlExecutor for FakeLocal {
        async fn query(
            &self,
            sql: &str,
            params: &[Value],
            _caller_role: Option<&str>,
        ) -> Result<Vec<Value>, ToolError> {
            let name = params
                .first()
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if sql.contains("synapse.connections") {
                return Ok(self
                    .connections
                    .lock()
                    .unwrap()
                    .get(&name)
                    .cloned()
                    .into_iter()
                    .collect());
            }
            if sql.contains("synapse.secrets") {
                return Ok(self
                    .secrets
                    .lock()
                    .unwrap()
                    .get(&name)
                    .map(|v| serde_json::json!({ "value": v }))
                    .into_iter()
                    .collect());
            }
            Ok(vec![])
        }
        async fn execute(
            &self,
            _sql: &str,
            _params: &[Value],
            _caller_role: Option<&str>,
        ) -> Result<u64, ToolError> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn resolve_connection_reports_a_clear_error_when_missing() {
        let local = FakeLocal::default();
        let err = resolve_connection(&local, "ghost", "remote_query")
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput { reason, .. } => {
                assert!(
                    reason.contains("ghost"),
                    "error should name the missing connection"
                );
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_connection_reads_host_port_dbname_user_and_secret() {
        let local = FakeLocal::default();
        local.connections.lock().unwrap().insert(
            "sales_db".into(),
            serde_json::json!({
                "host": "rds.example.internal",
                "port": 5433,
                "dbname": "sales",
                "user": "reporter",
                "password_secret": "sales_db_password",
            }),
        );
        local
            .secrets
            .lock()
            .unwrap()
            .insert("sales_db_password".into(), "s3cret".into());

        let info = resolve_connection(&local, "sales_db", "remote_query")
            .await
            .unwrap();
        assert_eq!(info.host, "rds.example.internal");
        assert_eq!(info.port, 5433);
        assert_eq!(info.dbname, "sales");
        assert_eq!(info.user, "reporter");
        assert_eq!(info.password.as_deref(), Some("s3cret"));
    }

    #[tokio::test]
    async fn resolve_connection_errors_when_secret_is_missing() {
        let local = FakeLocal::default();
        local.connections.lock().unwrap().insert(
            "sales_db".into(),
            serde_json::json!({
                "host": "h", "port": 5432, "dbname": "d", "user": "u",
                "password_secret": "does_not_exist",
            }),
        );
        let err = resolve_connection(&local, "sales_db", "remote_query")
            .await
            .unwrap_err();
        match err {
            ToolError::Execution { reason, .. } => {
                assert!(reason.contains("does_not_exist"));
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }
}
