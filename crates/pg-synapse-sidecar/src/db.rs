//! sqlx-backed implementations of [`ProfileSource`] and [`SqlExecutor`].
//!
//! ## SqlxProfileSource
//!
//! Reads `synapse.llm_profiles`, `synapse.embedding_profiles`,
//! `synapse.agents`, and `synapse.secrets` via sqlx, matching the column set
//! that `SpiProfileSource` reads inside the pgrx host. Uses runtime
//! `sqlx::query` (not `query!`) to avoid requiring a live DB at compile time.
//!
//! ## SqlxSqlExecutor
//!
//! Backs the `sql_query` / `sql_exec` tools. Binds positional JSON params the
//! same way the pgrx SpiSqlExecutor does (string, integer, float, bool, null,
//! object/array -> JSON text). The pool's own role runs every statement; per-
//! call SET ROLE is deferred to v0.2 (callers that need fine-grained privilege
//! separation should use the pgrx extension host instead).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use async_trait::async_trait;
use pg_synapse_core::error::{RuntimeError, ToolError};
use pg_synapse_core::runtime::ProfileSource;
use pg_synapse_core::types::{AgentRow, EmbeddingProfileRow, LlmProfileRow};
use pg_synapse_tools_sql::SqlExecutor;
use serde_json::Value;
use sqlx::{PgPool, Row};
use tracing::instrument;

// ---------------------------------------------------------------------------
// SqlxProfileSource
// ---------------------------------------------------------------------------

/// sqlx-backed ProfileSource. All four table reads happen at kernel build
/// time (startup); after that the Runtime is immutable. Restart the sidecar
/// to pick up schema changes.
pub struct SqlxProfileSource {
    pool: PgPool,
}

impl SqlxProfileSource {
    /// Construct from a live pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Map a sqlx error at row-decode time to a RuntimeError.
fn db_err(e: sqlx::Error) -> RuntimeError {
    RuntimeError::Config(format!("db row decode: {e}"))
}

/// Try to read a NUMERIC (cost_cap_usd) column as f64. sqlx without the
/// `decimal` feature cannot decode NUMERIC directly. We try f64 first (works
/// when the column fits), then fall back to parsing the text representation.
fn try_numeric_f64(row: &sqlx::postgres::PgRow, name: &str) -> Option<f64> {
    if let Ok(v) = row.try_get::<Option<f64>, _>(name) {
        return v;
    }
    if let Ok(Some(s)) = row.try_get::<Option<String>, _>(name) {
        return s.parse::<f64>().ok();
    }
    None
}

#[async_trait]
impl ProfileSource for SqlxProfileSource {
    #[instrument(skip(self), err)]
    async fn llm_profiles(&self) -> Result<Vec<LlmProfileRow>, RuntimeError> {
        let rows = sqlx::query(
            "SELECT name, provider, model, api_key_secret, base_url, params \
             FROM synapse.llm_profiles",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RuntimeError::Config(format!("llm_profiles query: {e}")))?;

        rows.iter()
            .map(|r| {
                let params_json: serde_json::Value = r
                    .try_get::<sqlx::types::Json<serde_json::Value>, _>("params")
                    .map(|j| j.0)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                Ok(LlmProfileRow {
                    name: r.try_get("name").map_err(db_err)?,
                    provider: r.try_get("provider").map_err(db_err)?,
                    model: r.try_get("model").map_err(db_err)?,
                    api_key_secret: r.try_get("api_key_secret").map_err(db_err)?,
                    base_url: r.try_get("base_url").map_err(db_err)?,
                    params: params_json,
                })
            })
            .collect()
    }

    #[instrument(skip(self), err)]
    async fn embedding_profiles(&self) -> Result<Vec<EmbeddingProfileRow>, RuntimeError> {
        let rows = sqlx::query(
            "SELECT name, provider, model, dimension, api_key_secret, base_url, params \
             FROM synapse.embedding_profiles",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RuntimeError::Config(format!("embedding_profiles query: {e}")))?;

        rows.iter()
            .map(|r| {
                let params_json: serde_json::Value = r
                    .try_get::<sqlx::types::Json<serde_json::Value>, _>("params")
                    .map(|j| j.0)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let dim: i32 = r.try_get("dimension").map_err(db_err)?;
                Ok(EmbeddingProfileRow {
                    name: r.try_get("name").map_err(db_err)?,
                    provider: r.try_get("provider").map_err(db_err)?,
                    model: r.try_get("model").map_err(db_err)?,
                    dimension: dim as u32,
                    api_key_secret: r.try_get("api_key_secret").map_err(db_err)?,
                    base_url: r.try_get("base_url").map_err(db_err)?,
                    params: params_json,
                })
            })
            .collect()
    }

    #[instrument(skip(self), err)]
    async fn agents(&self) -> Result<Vec<AgentRow>, RuntimeError> {
        let rows = sqlx::query(
            "SELECT name, system_prompt, soul, executor_name, \
                    llm_profile_main, llm_profile_small, llm_profile_judge, \
                    embedding_profile, tools, max_iterations, timeout_ms, \
                    cost_cap_usd::text as cost_cap_usd_text \
             FROM synapse.agents",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RuntimeError::Config(format!("agents query: {e}")))?;

        rows.iter()
            .map(|r| {
                let tools: Vec<String> = r.try_get("tools").map_err(db_err)?;
                let max_iter: i32 = r.try_get("max_iterations").map_err(db_err)?;
                let timeout: i64 = r.try_get("timeout_ms").map_err(db_err)?;
                // cost_cap_usd is NUMERIC; decode via text cast to avoid rust_decimal dep.
                let cost_cap: Option<f64> = r
                    .try_get::<Option<String>, _>("cost_cap_usd_text")
                    .ok()
                    .flatten()
                    .and_then(|s| s.parse::<f64>().ok());
                Ok(AgentRow {
                    name: r.try_get("name").map_err(db_err)?,
                    system_prompt: r.try_get("system_prompt").map_err(db_err)?,
                    soul: r.try_get("soul").map_err(db_err)?,
                    executor_name: r.try_get("executor_name").map_err(db_err)?,
                    llm_profile_main: r.try_get("llm_profile_main").map_err(db_err)?,
                    llm_profile_small: r.try_get("llm_profile_small").map_err(db_err)?,
                    llm_profile_judge: r.try_get("llm_profile_judge").map_err(db_err)?,
                    embedding_profile: r.try_get("embedding_profile").map_err(db_err)?,
                    tools,
                    max_iterations: max_iter as u32,
                    timeout_ms: timeout as u64,
                    cost_cap_usd: cost_cap,
                })
            })
            .collect()
    }

    #[instrument(skip(self), err)]
    async fn secrets(&self, names: &[&str]) -> Result<HashMap<String, String>, RuntimeError> {
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let name_vec: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let rows = sqlx::query("SELECT name, value FROM synapse.secrets WHERE name = ANY($1)")
            .bind(&name_vec)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| RuntimeError::Config(format!("secrets query: {e}")))?;

        let mut out = HashMap::new();
        for r in rows {
            let name: String = r.try_get("name").map_err(db_err)?;
            let value: String = r.try_get("value").map_err(db_err)?;
            out.insert(name, value);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// SqlxSqlExecutor
// ---------------------------------------------------------------------------

/// sqlx-backed SqlExecutor for the `sql_query` / `sql_exec` tools.
///
/// Binds JSON params positionally ($1, $2, ...) as Option<String>, relying on
/// Postgres implicit casts to interpret the text. This covers the agent use-
/// case (LLM-generated params are always simple scalars). A typed binding
/// layer (matching the pgrx SpiSqlExecutor's json_to_datum) is a v0.2
/// refinement.
///
/// Per-call SET ROLE (to impersonate `caller_role`) is deferred to v0.2.
pub struct SqlxSqlExecutor {
    pool: PgPool,
}

impl SqlxSqlExecutor {
    /// Construct from an existing pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Convert a JSON value to Option<String> for sqlx text binding.
/// Null maps to None (SQL NULL); everything else serializes to its text form.
fn json_to_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Object(_) | Value::Array(_) => Some(v.to_string()),
    }
}

/// Decode a single column into a JSON Value, trying types in order.
fn try_decode_column(row: &sqlx::postgres::PgRow, name: &str) -> serde_json::Value {
    // JSON/JSONB columns.
    if let Ok(v) = row.try_get::<sqlx::types::Json<serde_json::Value>, _>(name) {
        return v.0;
    }
    // Boolean.
    if let Ok(v) = row.try_get::<bool, _>(name) {
        return serde_json::Value::Bool(v);
    }
    // Integers (try i64 before i32 to handle bigint columns).
    if let Ok(v) = row.try_get::<i64, _>(name) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<i32, _>(name) {
        return serde_json::json!(v);
    }
    // Floats.
    if let Ok(v) = row.try_get::<f64, _>(name) {
        return serde_json::json!(v);
    }
    // UUID.
    if let Ok(v) = row.try_get::<uuid::Uuid, _>(name) {
        return serde_json::Value::String(v.to_string());
    }
    // Timestamp.
    if let Ok(v) = row.try_get::<chrono::DateTime<chrono::Utc>, _>(name) {
        return serde_json::Value::String(v.to_rfc3339());
    }
    // Text fallback.
    if let Ok(v) = row.try_get::<String, _>(name) {
        return serde_json::Value::String(v);
    }
    serde_json::Value::Null
}

fn rows_to_json(rows: Vec<sqlx::postgres::PgRow>) -> Result<Vec<serde_json::Value>, ToolError> {
    use sqlx::Column;
    rows.iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for col in row.columns() {
                let name = col.name();
                let val = try_decode_column(row, name);
                obj.insert(name.to_string(), val);
            }
            Ok(serde_json::Value::Object(obj))
        })
        .collect()
}

#[async_trait]
impl SqlExecutor for SqlxSqlExecutor {
    #[instrument(skip(self, params), fields(sql = %sql), err)]
    async fn query(
        &self,
        sql: &str,
        params: &[Value],
        _caller_role: Option<&str>,
    ) -> Result<Vec<Value>, ToolError> {
        // NOTE: caller_role (SET ROLE) is deferred to v0.2.
        if params.is_empty() {
            let rows =
                sqlx::query(sql)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| ToolError::Execution {
                        name: "sql_query".into(),
                        reason: e.to_string(),
                    })?;
            return rows_to_json(rows);
        }

        let mut q = sqlx::query(sql);
        for v in params {
            q = q.bind(json_to_text(v));
        }
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ToolError::Execution {
                name: "sql_query".into(),
                reason: e.to_string(),
            })?;
        rows_to_json(rows)
    }

    #[instrument(skip(self, params), fields(sql = %sql), err)]
    async fn execute(
        &self,
        sql: &str,
        params: &[Value],
        _caller_role: Option<&str>,
    ) -> Result<u64, ToolError> {
        // NOTE: caller_role (SET ROLE) is deferred to v0.2.
        if params.is_empty() {
            let result =
                sqlx::query(sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| ToolError::Execution {
                        name: "sql_exec".into(),
                        reason: e.to_string(),
                    })?;
            return Ok(result.rows_affected());
        }

        let mut q = sqlx::query(sql);
        for v in params {
            q = q.bind(json_to_text(v));
        }
        let result = q
            .execute(&self.pool)
            .await
            .map_err(|e| ToolError::Execution {
                name: "sql_exec".into(),
                reason: e.to_string(),
            })?;
        Ok(result.rows_affected())
    }
}

// Suppress dead_code for try_numeric_f64 which is available for future use.
#[allow(dead_code)]
fn _use_try_numeric_f64() {
    let _ = try_numeric_f64 as fn(&sqlx::postgres::PgRow, &str) -> Option<f64>;
}
