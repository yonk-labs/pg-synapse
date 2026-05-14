//! SPI-backed implementations of [`pg_synapse_core::ProfileSource`] (config
//! tables to kernel) and [`pg_synapse_tools_sql::SqlExecutor`] (built-in
//! `sql_query` / `sql_exec` tools).
//!
//! SPI is intrinsically synchronous: every call must run on the Postgres
//! backend thread that owns the current transaction. The async signatures
//! required by the trait don't introduce real I/O suspension; the kernel runs
//! these on a `current_thread` tokio runtime so polling stays on the backend
//! thread (see `runtime_holder`).

use std::collections::HashMap;

use async_trait::async_trait;
use pgrx::JsonB;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use serde_json::Value;

use pg_synapse_core::error::{RuntimeError, ToolError};
use pg_synapse_core::runtime::ProfileSource;
use pg_synapse_core::types::{AgentRow, EmbeddingProfileRow, LlmProfileRow};
use pg_synapse_tools_sql::SqlExecutor;

/// SQL executor that runs queries via SPI from inside a Postgres backend.
///
/// v0.1-alpha limitation: positional bind parameters (`$1`, `$2`, ...) are
/// not yet supported. The kernel-level SQL tools forward the agent's `params`
/// array and the executor rejects calls that supply any. The agent prompt
/// should instruct the LLM to inline literal values until full bind support
/// lands in M7-phase-B.
#[derive(Default)]
pub struct SpiSqlExecutor;

#[async_trait]
impl SqlExecutor for SpiSqlExecutor {
    async fn query(
        &self,
        sql: &str,
        params: &[Value],
        _caller_role: Option<&str>,
    ) -> Result<Vec<Value>, ToolError> {
        if crate::schema_guc::DISABLE_BUILTIN_SQL_TOOLS.get() {
            return Err(ToolError::Execution {
                name: "sql_query".into(),
                reason: "pg_synapse.disable_builtin_sql_tools is true".into(),
            });
        }
        if !params.is_empty() {
            return Err(ToolError::Execution {
                name: "sql_query".into(),
                reason: "positional params not yet supported in pgrx host (v0.1-alpha)".into(),
            });
        }

        // Wrap arbitrary SELECTs in row_to_jsonb so result shape is uniform.
        let wrapped = format!("SELECT to_jsonb(t) FROM ({sql}) t");

        Spi::connect(|client| -> Result<Vec<Value>, ToolError> {
            let table =
                client
                    .select(wrapped.as_str(), None, &[])
                    .map_err(|e| ToolError::Execution {
                        name: "sql_query".into(),
                        reason: e.to_string(),
                    })?;
            let mut out: Vec<Value> = Vec::new();
            for row in table {
                let v: Option<JsonB> = row.get(1).map_err(|e| ToolError::Execution {
                    name: "sql_query".into(),
                    reason: e.to_string(),
                })?;
                if let Some(JsonB(j)) = v {
                    out.push(j);
                }
            }
            Ok(out)
        })
    }

    async fn execute(
        &self,
        sql: &str,
        params: &[Value],
        _caller_role: Option<&str>,
    ) -> Result<u64, ToolError> {
        if crate::schema_guc::DISABLE_BUILTIN_SQL_TOOLS.get() {
            return Err(ToolError::Execution {
                name: "sql_exec".into(),
                reason: "pg_synapse.disable_builtin_sql_tools is true".into(),
            });
        }
        if !params.is_empty() {
            return Err(ToolError::Execution {
                name: "sql_exec".into(),
                reason: "positional params not yet supported in pgrx host (v0.1-alpha)".into(),
            });
        }

        Spi::connect_mut(|client| -> Result<u64, ToolError> {
            let table = client
                .update(sql, None, &[])
                .map_err(|e| ToolError::Execution {
                    name: "sql_exec".into(),
                    reason: e.to_string(),
                })?;
            Ok(table.len() as u64)
        })
    }
}

/// `ProfileSource` reading from the `pg_synapse.*` tables via SPI.
pub struct SpiProfileSource;

#[async_trait]
impl ProfileSource for SpiProfileSource {
    async fn llm_profiles(&self) -> Result<Vec<LlmProfileRow>, RuntimeError> {
        Spi::connect(|client| -> Result<Vec<LlmProfileRow>, RuntimeError> {
            let table = client
                .select(
                    "SELECT name, provider, model, api_key_secret, base_url, COALESCE(params, '{}'::jsonb) FROM synapse.llm_profiles",
                    None,
                    &[],
                )
                .map_err(|e| RuntimeError::Config(e.to_string()))?;
            let mut out = Vec::new();
            for row in table {
                out.push(LlmProfileRow {
                    name: row
                        .get::<String>(1)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    provider: row
                        .get::<String>(2)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    model: row
                        .get::<String>(3)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    api_key_secret: row.get::<String>(4).ok().flatten(),
                    base_url: row.get::<String>(5).ok().flatten(),
                    params: row
                        .get::<JsonB>(6)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .map(|j| j.0)
                        .unwrap_or(Value::Object(Default::default())),
                });
            }
            Ok(out)
        })
    }

    async fn embedding_profiles(&self) -> Result<Vec<EmbeddingProfileRow>, RuntimeError> {
        Spi::connect(|client| -> Result<Vec<EmbeddingProfileRow>, RuntimeError> {
            let table = client
                .select(
                    "SELECT name, provider, model, dimension, api_key_secret, base_url, COALESCE(params, '{}'::jsonb) FROM synapse.embedding_profiles",
                    None,
                    &[],
                )
                .map_err(|e| RuntimeError::Config(e.to_string()))?;
            let mut out = Vec::new();
            for row in table {
                out.push(EmbeddingProfileRow {
                    name: row
                        .get::<String>(1)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    provider: row
                        .get::<String>(2)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    model: row
                        .get::<String>(3)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    dimension: row.get::<i32>(4).ok().flatten().unwrap_or(0) as u32,
                    api_key_secret: row.get::<String>(5).ok().flatten(),
                    base_url: row.get::<String>(6).ok().flatten(),
                    params: row
                        .get::<JsonB>(7)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .map(|j| j.0)
                        .unwrap_or(Value::Object(Default::default())),
                });
            }
            Ok(out)
        })
    }

    async fn agents(&self) -> Result<Vec<AgentRow>, RuntimeError> {
        Spi::connect(|client| -> Result<Vec<AgentRow>, RuntimeError> {
            let table = client
                .select(
                    "SELECT name, system_prompt, soul, executor_name, llm_profile_main, llm_profile_small, llm_profile_judge, embedding_profile, tools, max_iterations, timeout_ms FROM synapse.agents",
                    None,
                    &[],
                )
                .map_err(|e| RuntimeError::Config(e.to_string()))?;
            let mut out = Vec::new();
            for row in table {
                out.push(AgentRow {
                    name: row
                        .get::<String>(1)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    system_prompt: row
                        .get::<String>(2)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_default(),
                    soul: row.get::<String>(3).ok().flatten(),
                    executor_name: row
                        .get::<String>(4)
                        .map_err(|e| RuntimeError::Config(e.to_string()))?
                        .unwrap_or_else(|| "conversation".into()),
                    llm_profile_main: row.get::<String>(5).ok().flatten(),
                    llm_profile_small: row.get::<String>(6).ok().flatten(),
                    llm_profile_judge: row.get::<String>(7).ok().flatten(),
                    embedding_profile: row.get::<String>(8).ok().flatten(),
                    tools: row.get::<Vec<String>>(9).ok().flatten().unwrap_or_default(),
                    max_iterations: row.get::<i32>(10).ok().flatten().unwrap_or(10) as u32,
                    timeout_ms: row.get::<i64>(11).ok().flatten().unwrap_or(60_000) as u64,
                    // cost_cap_usd: NUMERIC handling is deferred to M7-phase-B.
                    cost_cap_usd: None,
                });
            }
            Ok(out)
        })
    }

    async fn secrets(&self, names: &[&str]) -> Result<HashMap<String, String>, RuntimeError> {
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let names_owned: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
        Spi::connect(|client| -> Result<HashMap<String, String>, RuntimeError> {
            let arg: DatumWithOid<'_> = DatumWithOid::from(names_owned);
            let table = client
                .select(
                    "SELECT name, value FROM synapse.secrets WHERE name = ANY($1)",
                    None,
                    &[arg],
                )
                .map_err(|e| RuntimeError::Config(e.to_string()))?;
            let mut out = HashMap::new();
            for row in table {
                let n = row
                    .get::<String>(1)
                    .map_err(|e| RuntimeError::Config(e.to_string()))?
                    .unwrap_or_default();
                let v = row
                    .get::<String>(2)
                    .map_err(|e| RuntimeError::Config(e.to_string()))?
                    .unwrap_or_default();
                if !n.is_empty() {
                    out.insert(n, v);
                }
            }
            Ok(out)
        })
    }
}
