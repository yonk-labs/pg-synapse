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
use std::sync::atomic::{AtomicU64, Ordering};

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
/// Two integrity guarantees (v0.1.1, N1.2 + N1.3):
///
/// * **Positional bind params.** The agent's `params` JSON array is mapped to
///   typed Postgres bind parameters (`$1`, `$2`, ...) so the LLM never has to
///   inline literals into the SQL string. Removes an injection footgun.
/// * **SAVEPOINT per tool call.** Every `query` / `execute` runs inside its
///   own Postgres internal subtransaction (a savepoint). On success it is
///   released; on error it is rolled back. A failing tool call therefore
///   discards only its own partial writes and never an earlier tool call's
///   committed work. See [`with_savepoint`] for why an internal
///   subtransaction is required rather than the SQL `SAVEPOINT` statement.
#[derive(Default)]
pub struct SpiSqlExecutor;

/// Monotonic suffix for savepoint names so nested / sequential tool calls on
/// one connection never collide. The kernel is single-threaded per backend
/// today, but a unique name is cheap insurance.
static SAVEPOINT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Map one agent-supplied JSON value to a typed Postgres bind datum.
///
/// * string  -> TEXT
/// * integer -> INT8 (`bigint`)
/// * float   -> FLOAT8 (`double precision`)
/// * bool    -> BOOL
/// * null    -> typed NULL (TEXT NULL, a safe default)
/// * object / array -> JSONB (the value re-serialized)
///
/// The returned datum owns its data via the `String` / `JsonB` it wraps;
/// callers must keep the produced `Vec` alive for the duration of the SPI
/// call (the slice of `DatumWithOid` borrows from it).
fn json_to_datum(v: &Value) -> DatumWithOid<'static> {
    match v {
        Value::String(s) => DatumWithOid::from(s.clone()),
        Value::Bool(b) => DatumWithOid::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                DatumWithOid::from(i)
            } else if let Some(u) = n.as_u64() {
                // u64 that doesn't fit i64: widen through f64 rather than
                // overflow. Rare for agent-generated params.
                DatumWithOid::from(u as f64)
            } else {
                DatumWithOid::from(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::Null => DatumWithOid::null::<String>(),
        Value::Object(_) | Value::Array(_) => DatumWithOid::from(JsonB(v.clone())),
    }
}

/// Build a `Vec<DatumWithOid>` from the agent `params` array. Kept in a local
/// so the borrowed slice handed to SPI stays valid.
fn bind_args(params: &[Value]) -> Vec<DatumWithOid<'static>> {
    params.iter().map(json_to_datum).collect()
}

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

        // Wrap arbitrary SELECTs in to_jsonb so result shape is uniform. The
        // user's $1..$n placeholders pass straight through the superquery.
        let wrapped = format!("SELECT to_jsonb(t) FROM ({sql}) t");

        with_savepoint("sql_query", || {
            let args = bind_args(params);
            // `connect_mut` + `update` (not `connect` + `select`) on purpose:
            // a read-only SPI query reuses the ActiveSnapshot, and inside the
            // fresh internal subtransaction no XID is assigned yet, so pgrx
            // would run the SELECT read-only and miss rows written earlier in
            // the same outer transaction (an earlier tool call, or the
            // caller). `update` marks the statement mutable, which takes a
            // current snapshot and gives correct read-your-writes semantics.
            // A SELECT issued through `update` is otherwise harmless.
            Spi::connect_mut(|client| -> Result<Vec<Value>, ToolError> {
                let table = client.update(wrapped.as_str(), None, &args).map_err(|e| {
                    ToolError::Execution {
                        name: "sql_query".into(),
                        reason: e.to_string(),
                    }
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

        with_savepoint("sql_exec", || {
            let args = bind_args(params);
            Spi::connect_mut(|client| -> Result<u64, ToolError> {
                let table = client
                    .update(sql, None, &args)
                    .map_err(|e| ToolError::Execution {
                        name: "sql_exec".into(),
                        reason: e.to_string(),
                    })?;
                Ok(table.len() as u64)
            })
        })
    }
}

/// Run `body` inside a Postgres internal subtransaction (a savepoint), so a
/// tool failure rolls back only that tool call's writes.
///
/// ## Why an internal subtransaction, not an SQL `SAVEPOINT`
///
/// The kernel invokes the SQL tools from inside `synapse.execute(...)`, a
/// SECURITY DEFINER function. Postgres rejects the SQL `SAVEPOINT` /
/// `ROLLBACK TO SAVEPOINT` statements when issued from within a function
/// (they are only valid at the top transaction level or inside a
/// PROCEDURE). The supported mechanism in this position is a Postgres
/// *internal subtransaction*, the same primitive PL/pgSQL's
/// `BEGIN ... EXCEPTION` block uses. We drive it via the documented C API
/// (`BeginInternalSubTransaction` / `ReleaseCurrentSubTransaction` /
/// `RollbackAndReleaseCurrentSubTransaction`) wrapped by [`PgTryBuilder`],
/// which converts a Postgres `ereport(ERROR)` longjmp into a catchable Rust
/// path so we can roll the subtransaction back instead of unwinding past it.
///
/// ## Behaviour
///
/// * `body` returns `Ok` -> the subtransaction is released; its writes stay
///   (subject to the outer transaction).
/// * `body` returns `Err(ToolError)` (a soft, pgrx-reported error) -> the
///   subtransaction is rolled back; the `ToolError` is returned.
/// * `body` triggers a hard Postgres `ERROR` (e.g. a constraint violation)
///   -> the error is caught, the subtransaction is rolled back, and a
///   [`ToolError::Execution`] describing it is returned.
///
/// In every case, writes made by *earlier* tool calls are untouched, and a
/// failing call leaves none of its own partial writes behind.
///
/// The `SAVEPOINT_SEQ` counter only names the savepoint for diagnostics;
/// internal subtransactions nest by stack discipline regardless of name.
fn with_savepoint<T>(
    tool: &str,
    body: impl FnOnce() -> Result<T, ToolError> + std::panic::UnwindSafe,
) -> Result<T, ToolError> {
    use pgrx::PgTryBuilder;
    use pgrx::pg_sys;
    use pgrx::pg_sys::panic::CaughtError;

    let _n = SAVEPOINT_SEQ.fetch_add(1, Ordering::Relaxed);

    // Save the memory context and resource owner so we can restore them after
    // the subtransaction commits or aborts (Postgres swaps these during a
    // subxact and an aborted subxact leaves them pointing at freed state).
    #[allow(unsafe_code)]
    // SAFETY: reading the two well-known Postgres globals on the backend
    // thread, which is where SPI (and therefore this code) always runs.
    let (saved_cxt, saved_owner) =
        unsafe { (pg_sys::CurrentMemoryContext, pg_sys::CurrentResourceOwner) };

    #[allow(unsafe_code)]
    // SAFETY: opening an internal subtransaction on the backend thread, the
    // same primitive PL/pgSQL uses for its EXCEPTION blocks.
    unsafe {
        pg_sys::BeginInternalSubTransaction(std::ptr::null());
    }

    let outcome: Result<T, ToolError> = PgTryBuilder::new(body)
        .catch_others(|caught: CaughtError| {
            // A hard Postgres ERROR longjmped out of `body`; pgrx turned it into
            // this catchable cause. Map it to a ToolError; the subtransaction is
            // rolled back below.
            let reason = match &caught {
                CaughtError::PostgresError(e)
                | CaughtError::ErrorReport(e)
                | CaughtError::RustPanic { ereport: e, .. } => e.message().to_string(),
            };
            Err(ToolError::Execution {
                name: tool.to_string(),
                reason,
            })
        })
        .execute();

    // Release on success, roll back on any failure (soft ToolError or caught
    // hard error). Then restore the saved context / resource owner.
    #[allow(unsafe_code)]
    // SAFETY: closing the subtransaction we opened above, on the backend
    // thread, then restoring the globals we saved before opening it.
    unsafe {
        if outcome.is_ok() {
            pg_sys::ReleaseCurrentSubTransaction();
        } else {
            pg_sys::RollbackAndReleaseCurrentSubTransaction();
        }
        pg_sys::CurrentMemoryContext = saved_cxt;
        pg_sys::CurrentResourceOwner = saved_owner;
    }

    outcome
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
