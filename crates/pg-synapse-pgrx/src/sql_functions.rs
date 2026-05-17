//! Public SQL surface under the `synapse` schema.
//!
//! Every `#[pg_extern]` function inside the `synapse` module below becomes a
//! SQL function the operator can call from psql / their app. Functions that
//! mutate configuration (`agent_create`, `agent_drop`, `llm_profile_set`,
//! `secret_set`) invalidate the kernel cache so the next `execute()` call
//! sees the change.
//!
//! Note: the schema is `synapse`, not `pg_synapse`. Postgres reserves the
//! `pg_` prefix for system catalogs / built-in roles; extension authors
//! cannot create a `pg_synapse` schema or `pg_synapse_*` role. The GUC
//! namespace `pg_synapse.*` is fine (custom GUC prefixes are allowed).

use pgrx::prelude::*;

use pg_synapse_core::types::OutcomeStatus;

pub(crate) fn status_label(s: &OutcomeStatus) -> &'static str {
    match s {
        OutcomeStatus::Completed => "completed",
        OutcomeStatus::MaxIterations => "max_iterations",
        OutcomeStatus::TimedOut => "timed_out",
        OutcomeStatus::CostCapExceeded => "cost_cap_exceeded",
        OutcomeStatus::Errored(_) => "errored",
    }
}

pub(crate) fn role_str(r: &pg_synapse_core::types::Role) -> &'static str {
    use pg_synapse_core::types::Role;
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

pub(crate) fn log_execution(
    o: &pg_synapse_core::types::ExecutorOutcome,
    agent: &str,
    input: &str,
    caller: Option<&str>,
) -> Result<(), String> {
    use pgrx::JsonB;
    use pgrx::datum::DatumWithOid;

    let exec_id = o
        .messages
        .first()
        .map(|m| m.execution_id.to_string())
        .unwrap_or_default();
    if exec_id.is_empty() {
        return Ok(());
    }
    let status = status_label(&o.status).to_string();

    let args: Vec<DatumWithOid<'_>> = vec![
        DatumWithOid::from(exec_id.clone()),
        DatumWithOid::from(agent.to_string()),
        DatumWithOid::from(input.to_string()),
        DatumWithOid::from(o.output.clone()),
        DatumWithOid::from(status),
        DatumWithOid::from(o.tokens_in as i32),
        DatumWithOid::from(o.tokens_out as i32),
        // `synapse.executions.cost_usd` is `NUMERIC(12,6)`. Bind it as
        // `AnyNumeric` (built from the f64 via Postgres' `float8_numeric`) so
        // the stored value keeps the column's full 6-decimal precision rather
        // than the lossy float text round-trip the previous `f64` bind used.
        // If the conversion ever fails (non-finite f64), fall back to NULL
        // instead of poisoning the whole audit row.
        match o.cost_usd.and_then(|c| pgrx::AnyNumeric::try_from(c).ok()) {
            Some(n) => DatumWithOid::from(n),
            None => DatumWithOid::null::<pgrx::AnyNumeric>(),
        },
        DatumWithOid::from(o.duration_ms as i64),
        match caller {
            Some(c) => DatumWithOid::from(c.to_string()),
            None => DatumWithOid::null::<String>(),
        },
    ];

    Spi::run_with_args(
        "INSERT INTO synapse.executions (execution_id, agent_name, input, output, status, tokens_in, tokens_out, cost_usd, duration_ms, caller_role, finished_at) VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())",
        &args,
    )
    .map_err(|e| e.to_string())?;

    for m in &o.messages {
        let msg_args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(m.execution_id.to_string()),
            DatumWithOid::from(m.seq as i32),
            DatumWithOid::from(role_str(&m.role).to_string()),
            match &m.content {
                Some(c) => DatumWithOid::from(c.clone()),
                None => DatumWithOid::null::<String>(),
            },
            match &m.tool_call_id {
                Some(c) => DatumWithOid::from(c.clone()),
                None => DatumWithOid::null::<String>(),
            },
            match &m.tool_name {
                Some(c) => DatumWithOid::from(c.clone()),
                None => DatumWithOid::null::<String>(),
            },
            match &m.tool_input {
                Some(v) => DatumWithOid::from(JsonB(v.clone())),
                None => DatumWithOid::null::<JsonB>(),
            },
            match &m.tool_output {
                Some(v) => DatumWithOid::from(JsonB(v.clone())),
                None => DatumWithOid::null::<JsonB>(),
            },
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.messages (execution_id, seq, role, content, tool_call_id, tool_name, tool_input, tool_output) VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8)",
            &msg_args,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The `synapse` Postgres schema: every `#[pg_extern]` in this module lands
/// in `synapse.*`.
#[pg_schema]
pub(crate) mod synapse {
    use pgrx::JsonB;
    use pgrx::datum::DatumWithOid;
    use pgrx::prelude::*;
    use serde_json::json;

    use super::{log_execution, status_label};
    use crate::runtime_holder::{kernel_handle, rebuild_kernel, tokio};

    /// Run the named agent against `input`. Returns a JSON object with the
    /// agent's output, token / cost accounting, status, and a summary of the
    /// tool calls made. On error returns `{"error": "...", "status": "errored"}`
    /// instead of raising a Postgres error.
    #[pg_extern(security_definer, parallel_safe)]
    pub fn execute(agent_name: &str, input: &str) -> JsonB {
        let caller_role: Option<String> = Spi::get_one("SELECT current_user::text").ok().flatten();

        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => {
                return JsonB(json!({
                    "error": e,
                    "status": "errored",
                }));
            }
        };

        let outcome = tokio().block_on(async {
            kernel
                .execute_with_caller(agent_name, input, caller_role.clone())
                .await
        });

        match outcome {
            Ok(o) => {
                let exec_id = o
                    .messages
                    .first()
                    .map(|m| m.execution_id.to_string())
                    .unwrap_or_default();
                let _ = log_execution(&o, agent_name, input, caller_role.as_deref());
                JsonB(json!({
                    "execution_id": exec_id,
                    "output": o.output,
                    "status": status_label(&o.status),
                    "tokens_in": o.tokens_in,
                    "tokens_out": o.tokens_out,
                    "cost_usd": o.cost_usd,
                    "duration_ms": o.duration_ms,
                    "tool_calls": o.tool_calls.iter().map(|tc| json!({
                        "name": tc.name,
                        "args": tc.args,
                    })).collect::<Vec<_>>(),
                }))
            }
            Err(e) => JsonB(json!({
                "error": e.to_string(),
                "status": "errored",
            })),
        }
    }

    /// UPSERT an agent row. Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn agent_create(
        name: &str,
        system_prompt: &str,
        executor_name: &str,
        llm_profile_main: Option<&str>,
        tools: Vec<String>,
        max_iterations: i32,
        timeout_ms: i64,
    ) {
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(name.to_string()),
            DatumWithOid::from(system_prompt.to_string()),
            DatumWithOid::from(executor_name.to_string()),
            match llm_profile_main {
                Some(p) => DatumWithOid::from(p.to_string()),
                None => DatumWithOid::null::<String>(),
            },
            DatumWithOid::from(tools),
            DatumWithOid::from(max_iterations),
            DatumWithOid::from(timeout_ms),
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.agents (name, system_prompt, executor_name, llm_profile_main, tools, max_iterations, timeout_ms) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (name) DO UPDATE SET system_prompt = EXCLUDED.system_prompt, executor_name = EXCLUDED.executor_name, llm_profile_main = EXCLUDED.llm_profile_main, tools = EXCLUDED.tools, max_iterations = EXCLUDED.max_iterations, timeout_ms = EXCLUDED.timeout_ms, updated_at = now()",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// Delete an agent row.
    #[pg_extern(security_definer)]
    pub fn agent_drop(name: &str) {
        let args: Vec<DatumWithOid<'_>> = vec![DatumWithOid::from(name.to_string())];
        Spi::run_with_args("DELETE FROM synapse.agents WHERE name = $1", &args).unwrap();
        rebuild_kernel();
    }

    /// UPSERT an LLM profile row. Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn llm_profile_set(
        name: &str,
        provider: &str,
        model: &str,
        base_url: Option<&str>,
        api_key_secret: Option<&str>,
        params: JsonB,
    ) {
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(name.to_string()),
            DatumWithOid::from(provider.to_string()),
            DatumWithOid::from(model.to_string()),
            match base_url {
                Some(s) => DatumWithOid::from(s.to_string()),
                None => DatumWithOid::null::<String>(),
            },
            match api_key_secret {
                Some(s) => DatumWithOid::from(s.to_string()),
                None => DatumWithOid::null::<String>(),
            },
            DatumWithOid::from(params),
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.llm_profiles (name, provider, model, base_url, api_key_secret, params) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (name) DO UPDATE SET provider=EXCLUDED.provider, model=EXCLUDED.model, base_url=EXCLUDED.base_url, api_key_secret=EXCLUDED.api_key_secret, params=EXCLUDED.params, updated_at=now()",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// UPSERT a secret row. Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn secret_set(name: &str, value: &str) {
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(name.to_string()),
            DatumWithOid::from(value.to_string()),
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.secrets (name, value) VALUES ($1,$2) ON CONFLICT (name) DO UPDATE SET value=EXCLUDED.value, updated_at=now()",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// UPSERT an embedding profile row. Invalidates the kernel cache so the
    /// next `synapse.embed()` call sees the change.
    #[pg_extern(security_definer)]
    pub fn embedding_profile_set(
        name: &str,
        provider: &str,
        model: &str,
        dimension: i32,
        base_url: Option<&str>,
        params: JsonB,
    ) {
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(name.to_string()),
            DatumWithOid::from(provider.to_string()),
            DatumWithOid::from(model.to_string()),
            DatumWithOid::from(dimension),
            match base_url {
                Some(s) => DatumWithOid::from(s.to_string()),
                None => DatumWithOid::null::<String>(),
            },
            DatumWithOid::from(params),
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.embedding_profiles (name, provider, model, dimension, base_url, params) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (name) DO UPDATE SET provider=EXCLUDED.provider, model=EXCLUDED.model, dimension=EXCLUDED.dimension, base_url=EXCLUDED.base_url, params=EXCLUDED.params, updated_at=now()",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// Embed `text` using the named embedding profile (or the default profile
    /// when `profile_name` is NULL). Returns the raw embedding as
    /// `double precision[]`. Stores nothing.
    #[pg_extern(security_definer, parallel_safe)]
    pub fn embed(text: &str, profile_name: Option<&str>) -> Vec<f64> {
        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => pgrx::error!("embed: {e}"),
        };
        let result = tokio().block_on(async { kernel.embed(text, profile_name).await });
        match result {
            Ok(v) => v.into_inner().into_iter().map(|f| f as f64).collect(),
            Err(e) => pgrx::error!("embed error: {e}"),
        }
    }

    /// pg_synapse extension version.
    #[pg_extern(security_definer)]
    pub fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Force the kernel cache to rebuild on the next `execute()` call.
    /// Useful after manual edits to the config tables.
    #[pg_extern(name = "rebuild_kernel", security_definer)]
    pub fn rebuild_kernel_fn() {
        rebuild_kernel();
    }

    // ---- v0.1.1 N2.2: remaining SQL surface ----

    /// List every registered agent. Returns a JSONB array of objects
    /// `{name, executor_name, llm_profile_main, tools}`. A JSONB array (not a
    /// `TABLE`) keeps the pgrx 0.18 surface simple and is consistent with
    /// `tool_list` / `execution_status`.
    #[pg_extern(security_definer)]
    pub fn agent_list() -> JsonB {
        let rows = Spi::connect(|client| -> Result<Vec<serde_json::Value>, String> {
            let table = client
                .select(
                    "SELECT name, executor_name, llm_profile_main, tools FROM synapse.agents ORDER BY name",
                    None,
                    &[],
                )
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in table {
                out.push(json!({
                    "name": row.get::<String>(1).ok().flatten().unwrap_or_default(),
                    "executor_name": row.get::<String>(2).ok().flatten().unwrap_or_default(),
                    "llm_profile_main": row.get::<String>(3).ok().flatten(),
                    "tools": row.get::<Vec<String>>(4).ok().flatten().unwrap_or_default(),
                }));
            }
            Ok(out)
        });
        match rows {
            Ok(v) => JsonB(serde_json::Value::Array(v)),
            Err(e) => JsonB(json!({ "error": e })),
        }
    }

    /// List every registered tool. Returns a JSONB array of objects
    /// `{name, description, kind}` from `synapse.tools`.
    #[pg_extern(security_definer)]
    pub fn tool_list() -> JsonB {
        let rows = Spi::connect(|client| -> Result<Vec<serde_json::Value>, String> {
            let table = client
                .select(
                    "SELECT name, description, kind FROM synapse.tools ORDER BY name",
                    None,
                    &[],
                )
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in table {
                out.push(json!({
                    "name": row.get::<String>(1).ok().flatten().unwrap_or_default(),
                    "description": row.get::<String>(2).ok().flatten(),
                    "kind": row.get::<String>(3).ok().flatten().unwrap_or_default(),
                }));
            }
            Ok(out)
        });
        match rows {
            Ok(v) => JsonB(serde_json::Value::Array(v)),
            Err(e) => JsonB(json!({ "error": e })),
        }
    }

    /// UPSERT a row into `synapse.tools`. Registry metadata only; the kernel
    /// resolves the actual tool implementation from registered plugins.
    /// Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn tool_register(
        name: &str,
        description: &str,
        schema_json: JsonB,
        kind: default!(&str, "'manual'"),
        config: default!(JsonB, "'{}'"),
    ) {
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(name.to_string()),
            DatumWithOid::from(description.to_string()),
            DatumWithOid::from(schema_json),
            DatumWithOid::from(kind.to_string()),
            DatumWithOid::from(config),
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.tools (name, description, schema_json, kind, config) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (name) DO UPDATE SET description=EXCLUDED.description, schema_json=EXCLUDED.schema_json, kind=EXCLUDED.kind, config=EXCLUDED.config",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// Delete an LLM profile row. Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn llm_profile_drop(name: &str) {
        let args: Vec<DatumWithOid<'_>> = vec![DatumWithOid::from(name.to_string())];
        Spi::run_with_args("DELETE FROM synapse.llm_profiles WHERE name = $1", &args).unwrap();
        rebuild_kernel();
    }

    /// Delete an embedding profile row. Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn embedding_profile_drop(name: &str) {
        let args: Vec<DatumWithOid<'_>> = vec![DatumWithOid::from(name.to_string())];
        Spi::run_with_args(
            "DELETE FROM synapse.embedding_profiles WHERE name = $1",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// Delete a secret row. Invalidates the kernel cache.
    #[pg_extern(security_definer)]
    pub fn secret_drop(name: &str) {
        let args: Vec<DatumWithOid<'_>> = vec![DatumWithOid::from(name.to_string())];
        Spi::run_with_args("DELETE FROM synapse.secrets WHERE name = $1", &args).unwrap();
        rebuild_kernel();
    }

    /// Invoke a registered tool directly, bypassing the agent loop. For
    /// testing and operator introspection. The tool runs with a `ToolCtx`
    /// whose `caller_role` is the calling Postgres role. Returns the tool's
    /// output as JSONB, or `{"error": "...", "status": "errored"}`.
    #[pg_extern(security_definer)]
    pub fn tool_call(tool_name: &str, input: JsonB) -> JsonB {
        let caller_role: Option<String> = Spi::get_one("SELECT current_user::text").ok().flatten();

        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => {
                return JsonB(json!({ "error": e, "status": "errored" }));
            }
        };

        let result =
            tokio().block_on(async { kernel.call_tool(tool_name, input.0, caller_role).await });

        match result {
            Ok(v) => JsonB(v),
            Err(e) => JsonB(json!({ "error": e.to_string(), "status": "errored" })),
        }
    }

    /// Enqueue an agent run and return its execution id.
    ///
    /// v0.1.1 is **synchronous under the hood**: a true background worker
    /// requires a Postgres bgworker (SPI is only legal on the backend thread
    /// that owns the transaction; a spawned tokio task cannot SPI). So this
    /// runs the execution inline, records the row, and returns the id. The
    /// async contract (return a uuid; poll with `execution_status`) is
    /// preserved. Real background execution is deferred to v0.2. See
    /// `NOTES.md`.
    #[pg_extern(security_definer)]
    pub fn execute_async(agent_name: &str, input: &str) -> pgrx::Uuid {
        let caller_role: Option<String> = Spi::get_one("SELECT current_user::text").ok().flatten();

        // Pre-insert a 'queued' row keyed by a fresh id so a poller can see
        // the execution exists even if the run below fails hard.
        let queued_id = uuid::Uuid::new_v4();
        let queued_args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(queued_id.to_string()),
            DatumWithOid::from(agent_name.to_string()),
            DatumWithOid::from(input.to_string()),
            DatumWithOid::from("queued".to_string()),
            match caller_role.as_deref() {
                Some(c) => DatumWithOid::from(c.to_string()),
                None => DatumWithOid::null::<String>(),
            },
        ];
        let _ = Spi::run_with_args(
            "INSERT INTO synapse.executions (execution_id, agent_name, input, status, caller_role) VALUES ($1::uuid, $2, $3, $4, $5)",
            &queued_args,
        );

        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => {
                let args: Vec<DatumWithOid<'_>> = vec![
                    DatumWithOid::from(format!("kernel error: {e}")),
                    DatumWithOid::from(queued_id.to_string()),
                ];
                let _ = Spi::run_with_args(
                    "UPDATE synapse.executions SET status='errored', output=$1, finished_at=now() WHERE execution_id=$2::uuid",
                    &args,
                );
                return pgrx::Uuid::from_bytes(*queued_id.as_bytes());
            }
        };

        let outcome = tokio().block_on(async {
            kernel
                .execute_with_caller(agent_name, input, caller_role.clone())
                .await
        });

        match outcome {
            Ok(o) => {
                // The kernel minted its own execution_id for the messages.
                // Drop the placeholder 'queued' row and log the real outcome
                // (executions + messages) through the shared logger so the
                // sync and async paths produce identical audit rows.
                let del: Vec<DatumWithOid<'_>> = vec![DatumWithOid::from(queued_id.to_string())];
                let _ = Spi::run_with_args(
                    "DELETE FROM synapse.executions WHERE execution_id = $1::uuid",
                    &del,
                );
                let _ = log_execution(&o, agent_name, input, caller_role.as_deref());
                let real_id = o
                    .messages
                    .first()
                    .map(|m| m.execution_id)
                    .unwrap_or(queued_id);
                pgrx::Uuid::from_bytes(*real_id.as_bytes())
            }
            Err(e) => {
                let args: Vec<DatumWithOid<'_>> = vec![
                    DatumWithOid::from(e.to_string()),
                    DatumWithOid::from(queued_id.to_string()),
                ];
                let _ = Spi::run_with_args(
                    "UPDATE synapse.executions SET status='errored', output=$1, finished_at=now() WHERE execution_id=$2::uuid",
                    &args,
                );
                pgrx::Uuid::from_bytes(*queued_id.as_bytes())
            }
        }
    }

    // ---- Reactive triggers: T1 (ADR D14 / operator approval 2026-05-17) ----
    //
    // The synapse.* surface additions below are explicitly approved by ADR D14 and
    // the operator decision recorded 2026-05-17. They override the next-backlog
    // item N2.2 deferral for reactive triggers.

    /// Insert a job row into synapse.agent_queue and return the job_id.
    /// This is the fire-and-forget enqueue path: the INSERT commits with the
    /// calling transaction and the LLM never blocks the writer.
    #[pg_extern(security_definer)]
    pub fn enqueue(agent: &str, input: &str, source: default!(Option<&str>, "NULL")) -> pgrx::Uuid {
        let job_id = uuid::Uuid::new_v4();
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(job_id.to_string()),
            DatumWithOid::from(agent.to_string()),
            DatumWithOid::from(input.to_string()),
            match source {
                Some(s) => DatumWithOid::from(s.to_string()),
                None => DatumWithOid::null::<String>(),
            },
        ];
        Spi::run_with_args(
            "INSERT INTO synapse.agent_queue (job_id, agent, input, source) \
             VALUES ($1::uuid, $2, $3, $4)",
            &args,
        )
        .unwrap_or_else(|e| pgrx::error!("enqueue: {e}"));
        pgrx::Uuid::from_bytes(*job_id.as_bytes())
    }

    /// Claim up to `max_jobs` queued rows, run each agent synchronously, and
    /// write the result back. Returns the number of jobs processed.
    ///
    /// Concurrency-safe: uses `FOR UPDATE SKIP LOCKED` so multiple concurrent
    /// drain callers each pick a disjoint set. Idempotent: a job that was
    /// marked 'running' by a crashed caller is left in 'running'; a future
    /// stuck-job reaper (v0.2) can detect and retry those.
    #[pg_extern(security_definer)]
    pub fn drain_queue(max_jobs: default!(i32, "10")) -> i32 {
        // Atomic claim: a single UPDATE whose subquery does the
        // FOR UPDATE SKIP LOCKED selection. An UPDATE is unambiguously a
        // write, so this avoids the "SELECT FOR UPDATE not allowed in a
        // non-volatile function" rejection that a standalone locking
        // SELECT triggers in the SPI context. RETURNING gives us the
        // claimed rows already marked 'running'.
        let claimed: Vec<(uuid::Uuid, String, String)> = Spi::connect_mut(|client| {
            // Data-modifying CTE: the UPDATE (with its FOR UPDATE SKIP
            // LOCKED subquery) does the atomic claim; the outer SELECT
            // over the CTE returns the claimed rows reliably as a
            // readable tuptable.
            let table = client
                .update(
                    "WITH claimed AS ( \
                       UPDATE synapse.agent_queue \
                       SET status='running', started_at=now() \
                       WHERE job_id IN ( \
                         SELECT job_id FROM synapse.agent_queue \
                         WHERE status='queued' \
                         ORDER BY enqueued_at \
                         LIMIT $1 \
                         FOR UPDATE SKIP LOCKED \
                       ) \
                       RETURNING job_id, agent, input \
                     ) \
                     SELECT job_id, agent, input FROM claimed",
                    None,
                    &[DatumWithOid::from(max_jobs)],
                )
                .map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            for row in table {
                let id_str: Option<String> = row.get(1).ok().flatten();
                let agent: Option<String> = row.get(2).ok().flatten();
                let input: Option<String> = row.get(3).ok().flatten();
                if let (Some(id_s), Some(a), Some(i)) = (id_str, agent, input) {
                    if let Ok(uid) = uuid::Uuid::parse_str(&id_s) {
                        rows.push((uid, a, i));
                    }
                }
            }
            Ok::<_, String>(rows)
        })
        .unwrap_or_default();

        let mut processed = 0i32;
        for (job_id, agent, input) in claimed {
            let job_str = job_id.to_string();

            // Already marked 'running' by the atomic claim above; the
            // legacy per-row mark is kept harmless for older callers.
            let upd_args: Vec<DatumWithOid<'_>> = vec![DatumWithOid::from(job_str.clone())];
            let _ = Spi::run_with_args(
                "UPDATE synapse.agent_queue \
                 SET status='running', started_at=COALESCE(started_at, now()) \
                 WHERE job_id=$1::uuid",
                &upd_args,
            );

            // Re-use the existing execute path (calls into the kernel).
            let result_jsonb = execute(&agent, &input);

            // Determine done vs error from the returned envelope.
            let (new_status, result_val, error_val) = {
                let v = &result_jsonb.0;
                if v.get("error").is_some() {
                    let err_str = v
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    ("error", None::<serde_json::Value>, Some(err_str))
                } else {
                    ("done", Some(v.clone()), None)
                }
            };

            let fin_args: Vec<DatumWithOid<'_>> = vec![
                DatumWithOid::from(new_status.to_string()),
                match result_val {
                    Some(ref rv) => DatumWithOid::from(JsonB(rv.clone())),
                    None => DatumWithOid::null::<JsonB>(),
                },
                match error_val {
                    Some(ref ev) => DatumWithOid::from(ev.clone()),
                    None => DatumWithOid::null::<String>(),
                },
                DatumWithOid::from(job_str),
            ];
            let _ = Spi::run_with_args(
                "UPDATE synapse.agent_queue \
                 SET status=$1, result=$2, error=$3, finished_at=now() \
                 WHERE job_id=$4::uuid",
                &fin_args,
            );
            processed += 1;
        }
        processed
    }

    /// Generate a row-level AFTER trigger and trigger function on `target_table`
    /// that fires an agent on each qualifying row.
    ///
    /// mode = 'queue'  (default): calls synapse.enqueue (async, never blocks
    ///                 the writer, the triggering INSERT/UPDATE always commits).
    /// mode = 'inline': calls synapse.execute synchronously inside the writing
    ///                 transaction. If the agent errors or returns
    ///                 `{"decision":"reject"}`, the trigger RAISEs and the
    ///                 triggering write rolls back.
    ///
    /// Recursion guard: `pg_trigger_depth() > 1` skips the trigger body so an
    /// agent's sql_exec writing back to the same table does not re-fire
    /// endlessly (ADR D14, operator approval 2026-05-17).
    ///
    /// Identifier safety: table name, function name, and trigger name are
    /// injected via `format(%I)` in the generated SQL, not via string concat.
    #[pg_extern(security_definer)]
    pub fn attach_agent_trigger(
        target_table: &str,
        agent: &str,
        mode: default!(&str, "'queue'"),
        events: default!(&str, "'INSERT'"),
        when_sql: default!(Option<&str>, "NULL"),
        input_expr: default!(&str, "'NEW::text'"),
    ) {
        // Derive stable function/trigger names from the table name.
        // Use underscores to create a valid identifier from "schema.table".
        let safe_name = target_table.replace('.', "_").replace('"', "");
        let fn_name = format!("synapse_trig_{safe_name}");
        let trig_name = format!("synapse_agent_{safe_name}");

        let when_clause = match when_sql {
            Some(w) => format!("WHEN ({w})"),
            None => String::new(),
        };

        // Pre-compute the SQL-quoted agent literal to avoid format! nesting.
        let agent_lit = format!("'{}'", agent.replace('\'', "''"));
        let table_lit = target_table.replace('\'', "''");

        let body = match mode {
            "inline" => {
                // Inline mode: call execute synchronously. Raise on error or
                // reject decision so the triggering statement rolls back.
                format!(
                    r#"
DECLARE
  _res JSONB;
  _status TEXT;
  _out TEXT;
  _obj JSONB;
  _decision TEXT;
  _reason TEXT;
BEGIN
  IF pg_trigger_depth() > 1 THEN
    RETURN NEW;
  END IF;
  _res := synapse.execute({agent_lit}, ({input_expr})::text);
  _status := _res->>'status';
  -- The envelope output is the agent reply TEXT, not a JSON object.
  -- Pull the first JSON object substring out of it and parse a decision.
  _out := _res->>'output';
  BEGIN
    _obj := substring(_out from '\{{[\s\S]*\}}')::jsonb;
    _decision := lower(_obj->>'decision');
  EXCEPTION WHEN others THEN
    _obj := NULL;
    _decision := NULL;
  END;
  IF _status IS DISTINCT FROM 'completed' THEN
    _reason := COALESCE(_res->>'error', _out, 'agent did not complete');
    RAISE EXCEPTION 'synapse inline trigger rejected: %', _reason;
  END IF;
  IF _decision = 'reject'
     OR (_decision IS NULL AND _out ~* '"decision"\s*:\s*"reject"') THEN
    _reason := COALESCE(
      _obj->>'reason',
      substring(_out from '"reason"\s*:\s*"([^"]*)"'),
      _out,
      'agent rejected row'
    );
    RAISE EXCEPTION 'synapse inline trigger rejected: %', _reason;
  END IF;
  RETURN NEW;
END;"#,
                    agent_lit = agent_lit,
                    input_expr = input_expr,
                )
            }
            _ => {
                // Queue mode (default): fire-and-forget enqueue.
                format!(
                    r#"
BEGIN
  IF pg_trigger_depth() > 1 THEN
    RETURN NEW;
  END IF;
  PERFORM synapse.enqueue(
    {agent_lit},
    ({input_expr})::text,
    'trigger:{table_lit}'
  );
  RETURN NEW;
END;"#,
                    agent_lit = agent_lit,
                    input_expr = input_expr,
                    table_lit = table_lit,
                )
            }
        };

        // Build and execute the trigger function + trigger via SPI.
        // Identifiers (fn_name, trig_name, target_table) go through %I in
        // format() inside the SQL so Postgres quotes them safely.
        let create_fn_sql = format!(
            "CREATE OR REPLACE FUNCTION {fn_name}() \
             RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $trig_body${body}$trig_body$",
        );

        Spi::run(&create_fn_sql)
            .unwrap_or_else(|e| pgrx::error!("attach_agent_trigger create function: {e}"));

        // Drop any existing trigger with this name on the table first (idempotent).
        let drop_sql = "SELECT format('DROP TRIGGER IF EXISTS %I ON %s', $1, $2::regclass)";
        if let Ok(Some(drop_stmt)) = Spi::get_one_with_args::<String>(
            drop_sql,
            &[
                DatumWithOid::from(trig_name.clone()),
                DatumWithOid::from(target_table.to_string()),
            ],
        ) {
            let _ = Spi::run(&drop_stmt);
        }

        let create_trig_sql = format!(
            "SELECT format('CREATE TRIGGER %I AFTER {events} ON %s \
             FOR EACH ROW {when_clause} EXECUTE FUNCTION {fn_name}()', $1, $2::regclass)",
            events = events,
            when_clause = when_clause,
            fn_name = fn_name,
        );
        let trig_stmt: Option<String> = Spi::get_one_with_args(
            &create_trig_sql,
            &[
                DatumWithOid::from(trig_name.clone()),
                DatumWithOid::from(target_table.to_string()),
            ],
        )
        .unwrap_or_else(|e| pgrx::error!("attach_agent_trigger format trigger: {e}"));

        if let Some(stmt) = trig_stmt {
            Spi::run(&stmt)
                .unwrap_or_else(|e| pgrx::error!("attach_agent_trigger create trigger: {e}"));
        }
    }

    /// Remove the trigger and trigger function previously created by
    /// `synapse.attach_agent_trigger` for `target_table`.
    #[pg_extern(security_definer)]
    pub fn detach_agent_trigger(target_table: &str) {
        let safe_name = target_table.replace('.', "_").replace('"', "");
        let fn_name = format!("synapse_trig_{safe_name}");
        let trig_name = format!("synapse_agent_{safe_name}");

        // Drop trigger.
        let drop_trig_sql = "SELECT format('DROP TRIGGER IF EXISTS %I ON %s', $1, $2::regclass)";
        if let Ok(Some(drop_stmt)) = Spi::get_one_with_args::<String>(
            drop_trig_sql,
            &[
                DatumWithOid::from(trig_name),
                DatumWithOid::from(target_table.to_string()),
            ],
        ) {
            let _ = Spi::run(&drop_stmt);
        }

        // Drop function.
        let drop_fn_sql = format!("DROP FUNCTION IF EXISTS {fn_name}()");
        Spi::run(&drop_fn_sql)
            .unwrap_or_else(|e| pgrx::error!("detach_agent_trigger drop function: {e}"));
    }

    /// Poll an execution by id. Returns
    /// `{status, output, tokens_in, tokens_out, cost_usd, duration_ms}` or
    /// `{"status": "not_found"}` when the id is unknown.
    #[pg_extern(security_definer)]
    pub fn execution_status(execution_id: pgrx::Uuid) -> JsonB {
        let id = uuid::Uuid::from_bytes(*execution_id.as_bytes()).to_string();
        let result = Spi::connect(|client| -> Result<Option<serde_json::Value>, String> {
            let arg: DatumWithOid<'_> = DatumWithOid::from(id);
            let table = client
                .select(
                    "SELECT status, output, tokens_in, tokens_out, cost_usd, duration_ms FROM synapse.executions WHERE execution_id = $1::uuid",
                    None,
                    &[arg],
                )
                .map_err(|e| e.to_string())?;
            match table.into_iter().next() {
                Some(row) => {
                    let cost = row
                        .get::<pgrx::AnyNumeric>(5)
                        .ok()
                        .flatten()
                        .and_then(|n| f64::try_from(n).ok());
                    Ok(Some(json!({
                        "status": row.get::<String>(1).ok().flatten().unwrap_or_default(),
                        "output": row.get::<String>(2).ok().flatten(),
                        "tokens_in": row.get::<i32>(3).ok().flatten().unwrap_or(0),
                        "tokens_out": row.get::<i32>(4).ok().flatten().unwrap_or(0),
                        "cost_usd": cost,
                        "duration_ms": row.get::<i64>(6).ok().flatten(),
                    })))
                }
                None => Ok(None),
            }
        });
        match result {
            Ok(Some(v)) => JsonB(v),
            Ok(None) => JsonB(json!({ "status": "not_found" })),
            Err(e) => JsonB(json!({ "error": e, "status": "errored" })),
        }
    }
}
