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
        match o.cost_usd {
            Some(c) => DatumWithOid::from(c),
            None => DatumWithOid::null::<f64>(),
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
}
