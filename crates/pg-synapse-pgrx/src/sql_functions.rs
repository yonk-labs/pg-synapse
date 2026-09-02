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

use crate::audit_capability::AuditGrant;

/// Warn when the encryption key is configured but reachable by any role.
///
/// `pg_synapse.master_key` is registered `SUPERUSER_ONLY`, which is what stops
/// a caller reading it. That flag only exists once this library has loaded,
/// and a custom GUC belonging to a library that has not loaded is an unflagged
/// **placeholder**: Postgres accepts it from the configuration and hands its
/// value to anyone who asks. So a key set in `postgresql.conf` or by
/// `ALTER SYSTEM`, without preloading, is readable by a fresh session that
/// never touches `synapse.*`. Measured, not theorised: `synapse_user` read a
/// key set by `ALTER SYSTEM` on an otherwise correctly configured database.
///
/// `shared_preload_libraries = 'pg_synapse_pgrx'` closes it, because the GUC is
/// then defined with its flag at postmaster start, before any session exists.
///
/// Checked over SPI rather than by reading
/// `process_shared_preload_libraries_in_progress`, which would need a second
/// `unsafe` site in a crate that documents exactly one.
///
/// A warning rather than an error: refusing to run would take a database down
/// over a configuration the operator may have deliberately accepted, and the
/// key is already exposed by the time we could complain.
pub(crate) fn warn_if_master_key_is_exposed() {
    if master_key().is_none() {
        return;
    }
    let preloaded: bool =
        Spi::get_one::<String>("SELECT current_setting('shared_preload_libraries', true)")
            .ok()
            .flatten()
            .is_some_and(|v| v.split(',').any(|l| l.trim() == "pg_synapse_pgrx"));
    if !preloaded {
        pgrx::warning!(
            "pg_synapse.master_key is set but pg_synapse_pgrx is not in \
             shared_preload_libraries; until it is, the key is an unflagged GUC \
             placeholder that any role can read with current_setting() before \
             this library loads. See docs/threat-model.md"
        );
    }
}

/// Whether a tool name reaches the network, directly or through another agent.
///
/// D5 refuses these in inline trigger mode. A name list rather than a trait
/// method on `Tool`, because the alternative is a new required method on every
/// plugin in the workspace to answer a question only this host asks. The
/// ceiling is that a plugin added later has to be added here too; the upgrade
/// path, if that ever bites, is a `Tool::reaches_network()` default-false
/// method the plugins that need it override.
///
/// `call_agent` is in the list for transitive reach: it runs another agent
/// through the kernel rather than through this function, so the trigger-depth
/// check never fires again and the inner agent's own tools decide what happens
/// next. Refusing it is broader than strictly necessary (an inner agent with
/// no network tools would be harmless) and deliberately so: inline mode exists
/// for a single bounded decision, and delegating to another agent inside
/// somebody's open write transaction is not that shape.
pub(crate) fn is_egress_tool(name: &str) -> bool {
    matches!(
        name,
        "http_get"
            | "http_head"
            | "http_post"
            | "search_news"
            | "fetch_feed"
            | "read_article"
            | "load_url"
            | "remote_query"
            | "remote_exec"
            | "call_agent"
    )
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

fn resolve_trace_level(agent_name: &str) -> pg_synapse_core::types::TraceLevel {
    use pgrx::datum::DatumWithOid;
    // Through the definer function, not the table: `synapse.agents` is not
    // readable by `synapse_user`, and this runs on every run.
    let level_str: Option<String> = pgrx::Spi::connect(|client| {
        client
            .select(
                "SELECT synapse.agent_trace_level($1)",
                None,
                &[DatumWithOid::from(agent_name.to_string())],
            )
            .ok()
            .and_then(|mut t| t.next().and_then(|r| r.get::<String>(1).ok().flatten()))
    });
    level_str.and_then(|s| s.parse().ok()).unwrap_or_default()
}

/// Record a run that failed before producing an outcome.
///
/// `log_execution` needs an `ExecutorOutcome`, which a failed run never
/// produces, so failures used to write nothing to `synapse.executions` at all:
/// cost cap trips, kernel build failures, provider errors, and endpoint
/// outages all vanished. That left the audit table containing only runs that
/// succeeded, which quietly over-reports success and defeats the point of
/// having it.
///
/// Best effort by design: a failure to record a failure must not itself
/// become an error the caller sees.
/// The configured secret-encryption key, if any.
///
/// Returned as an owned String because the GUC is read once per call and the
/// value must not outlive it. `None` means encryption is off, which is what
/// every install that has not opted in will see.
pub(crate) fn master_key() -> Option<String> {
    crate::schema_guc::MASTER_KEY
        .get()
        .and_then(|c: std::ffi::CString| c.into_string().ok())
        .filter(|k: &String| !k.trim().is_empty())
}

pub(crate) fn log_failed_execution(
    agent: &str,
    input: &str,
    error: &str,
    caller: Option<&str>,
    grant: &AuditGrant,
) {
    let _ = record_status(
        &uuid::Uuid::new_v4().to_string(),
        agent,
        Some(input),
        Some(error),
        "errored",
        caller,
        grant,
    );
}

/// Write one `synapse.executions` row at `status`, through the definer
/// boundary. Upserts, so the same call both plants the async path's `queued`
/// placeholder and later turns it into the terminal row.
pub(crate) fn record_status(
    execution_id: &str,
    agent: &str,
    input: Option<&str>,
    output: Option<&str>,
    status: &str,
    caller: Option<&str>,
    grant: &AuditGrant,
) -> Result<(), String> {
    use pgrx::JsonB;
    use pgrx::datum::DatumWithOid;
    let doc = serde_json::json!({
        "execution_id": execution_id,
        "agent_name": agent,
        "input": input,
        "output": output,
        "status": status,
        "caller_role": caller,
    });
    Spi::run_with_args(
        "SELECT synapse.audit_status($1, $2)",
        &[
            DatumWithOid::from(JsonB(doc)),
            DatumWithOid::from(token_text(grant)),
        ],
    )
    .map_err(|e| e.to_string())
}

/// The capability token as the text the SQL layer takes.
///
/// Hex rather than decimal only because it is shorter; the value is never
/// parsed for meaning, only compared. This is the single place the token
/// becomes a string, and it must not be used anywhere it could be logged.
fn token_text(grant: &AuditGrant) -> String {
    format!("{:032x}", grant.token())
}

pub(crate) fn log_execution(
    o: &pg_synapse_core::types::ExecutorOutcome,
    agent: &str,
    input: &str,
    caller: Option<&str>,
    trace_level: pg_synapse_core::types::TraceLevel,
    // The async path writes a placeholder row under its own id before running,
    // then the kernel mints a different id for the messages. Passing it here
    // lets the replacement happen inside one statement instead of a DELETE
    // that could leave the run with no row at all if the write after it failed.
    supersedes: Option<&str>,
    // Proof this write belongs to a run an entry point actually started. See
    // `audit_capability`.
    grant: &AuditGrant,
) -> Result<(), String> {
    use pgrx::JsonB;
    use pgrx::datum::DatumWithOid;
    use serde_json::json;

    let exec_id = o
        .messages
        .first()
        .map(|m| m.execution_id.to_string())
        // A run that ended before producing any message (a provider that
        // failed on the very first call) has no message to take an id from.
        // Falling back to a fresh uuid keeps the row insertable: previously
        // the empty string failed the ::uuid cast, the error was discarded by
        // the caller's `let _ =`, and the run vanished from the audit trail
        // entirely. A run that ended badly is exactly the one that must not
        // disappear.
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if exec_id.is_empty() {
        return Ok(());
    }

    let run_succeeded = o.status == pg_synapse_core::types::OutcomeStatus::Completed;
    let messages = if trace_level.should_persist_messages(run_succeeded) {
        o.messages
            .iter()
            .map(|m| {
                json!({
                    "seq": m.seq,
                    "role": role_str(&m.role),
                    "content": m.content,
                    "tool_call_id": m.tool_call_id,
                    "tool_name": m.tool_name,
                    "tool_input": m.tool_input,
                    "tool_output": m.tool_output,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // D6: fill the already-decided `synapse.traces` writer (do not redesign
    // the schema). D8: persist + pollable only, no live push. Events are
    // persisted only at trace_level >= debug; `seq` is the event's ordinal
    // within the run.
    let events: Vec<serde_json::Value> = if trace_level.should_persist_events() {
        o.events
            .iter()
            .enumerate()
            .map(
                |(seq, ev)| json!({ "seq": seq, "event": ev.kind.as_str(), "payload": ev.payload }),
            )
            .collect()
    } else {
        Vec::new()
    };

    // `cost_usd` lands in a NUMERIC(12,6). Serializing the f64 into the jsonb
    // document and casting on the SQL side keeps the column's full precision
    // without the lossy float-to-text round trip a text bind would cost. A
    // non-finite f64 has no json number to serialize to, so it becomes null
    // rather than poisoning the whole audit row.
    let cost = o
        .cost_usd
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null);

    let mut doc = json!({
        "execution": {
            "execution_id": exec_id,
            "agent_name": agent,
            "input": input,
            "output": o.output,
            "status": status_label(&o.status),
            "tokens_in": o.tokens_in,
            "tokens_out": o.tokens_out,
            "cost_usd": cost,
            "duration_ms": o.duration_ms,
            "caller_role": caller,
        },
        "messages": messages,
        "events": events,
    });
    if let Some(id) = supersedes {
        doc["supersedes"] = serde_json::Value::String(id.to_owned());
    }

    // One privileged call for the whole run. It used to be one INSERT per
    // message and per event, so a 23 message run cost 24 SPI round trips; and
    // routing them through a single SECURITY DEFINER function is what lets the
    // entry points drop to the caller's own rights without taking the audit
    // trail down with them (F2).
    Spi::run_with_args(
        "SELECT synapse.audit_run($1, $2)",
        &[
            DatumWithOid::from(JsonB(doc)),
            DatumWithOid::from(token_text(grant)),
        ],
    )
    .map_err(|e| e.to_string())
}

/// The `synapse` Postgres schema: every `#[pg_extern]` in this module lands
/// in `synapse.*`.
#[pg_schema]
pub(crate) mod synapse {
    use pgrx::JsonB;
    use pgrx::datum::DatumWithOid;
    use pgrx::prelude::*;
    use serde_json::json;

    use super::{log_execution, log_failed_execution, resolve_trace_level, status_label};
    use crate::audit_capability::AuditGrant;
    use crate::runtime_holder::{kernel_handle, rebuild_kernel, tokio};

    /// Run the named agent against `input`. Returns a JSON object with the
    /// agent's output, token / cost accounting, status, and a summary of the
    /// tool calls made. On error returns `{"error": "...", "status": "errored"}`
    /// instead of raising a Postgres error.
    #[pg_extern(parallel_safe)]
    pub fn execute(agent_name: &str, input: &str) -> JsonB {
        // `role`, not `current_user`: see `tool_call`.
        let caller_role: Option<String> = Spi::get_one(
            "SELECT COALESCE(NULLIF(current_setting('role', true), 'none'), session_user::text)",
        )
        .ok()
        .flatten();

        // Live for the rest of this run and retired when it drops, which is
        // what lets the audit writers be granted to a caller role without
        // becoming a way to forge rows. See `audit_capability`.
        let grant = AuditGrant::mint();

        // D5 / O1: inline trigger mode is not a separate entry point, it is
        // this one reached from inside a trigger. Queue mode triggers call
        // `synapse.enqueue`, so an `execute` at trigger depth is by definition
        // running inside the writer's open transaction. Detecting it here
        // rather than generating a different call means a trigger attached
        // before this existed is covered too, as is a hand-written one.
        let inline = Spi::get_one::<i32>("SELECT pg_trigger_depth()")
            .ok()
            .flatten()
            .unwrap_or(0)
            > 0;

        // SECURITY INVOKER from here on, so `kernel_handle()` in process would
        // do its config reads with the caller's rights and fail on
        // synapse.agents. Crossing into the definer `ensure_kernel` is what
        // earns the owner's rights for exactly those reads; afterwards the
        // cache is populated and `kernel_handle()` touches no table.
        if let Err(e) = Spi::run("SELECT synapse.ensure_kernel()") {
            return JsonB(json!({ "error": e.to_string(), "status": "errored" }));
        }

        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => {
                return JsonB(json!({
                    "error": e,
                    "status": "errored",
                }));
            }
        };

        // D5: network tools are refused inline. An outbound HTTP call inside a
        // write transaction holds the writer's locks for the duration of
        // somebody else's outage, which is not defensible at any timeout. This
        // refuses the run rather than the tool call, so the failure arrives
        // before any lock is taken rather than part way through.
        if inline {
            if let Some(bad) = kernel
                .agents()
                .find(|a| a.name == agent_name)
                .and_then(|a| a.tools.iter().find(|t| super::is_egress_tool(t)))
            {
                return JsonB(json!({
                    "error": format!(
                        "agent '{agent_name}' has the network tool '{bad}' and cannot run in \
                         inline trigger mode, which holds the writing transaction open for the \
                         whole run; attach it in queue mode instead"
                    ),
                    "status": "errored",
                }));
            }
        }

        let outcome = tokio().block_on(async {
            if inline {
                // Only ever lowers: an agent configured tighter than the
                // ceiling keeps its own budget.
                let cap = std::time::Duration::from_millis(
                    crate::schema_guc::INLINE_TIMEOUT_MS.get().max(1) as u64,
                );
                kernel
                    .execute_with_budget(agent_name, input, caller_role.clone(), cap)
                    .await
            } else {
                kernel
                    .execute_with_caller(agent_name, input, caller_role.clone())
                    .await
            }
        });

        match outcome {
            Ok(o) => {
                let exec_id = o
                    .messages
                    .first()
                    .map(|m| m.execution_id.to_string())
                    .unwrap_or_default();
                let tl = resolve_trace_level(agent_name);
                // Not `let _ =`: a failure to record a run used to be silent,
                // which is how a timed-out run went missing without anyone
                // noticing. It still must not fail the caller's query, so it
                // is reported as a warning rather than raised.
                if let Err(e) = log_execution(
                    &o,
                    agent_name,
                    input,
                    caller_role.as_deref(),
                    tl,
                    None,
                    &grant,
                ) {
                    pgrx::warning!("could not record execution for agent {agent_name}: {e}");
                }
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
            Err(e) => {
                let msg = e.to_string();
                log_failed_execution(agent_name, input, &msg, caller_role.as_deref(), &grant);
                JsonB(json!({
                    "error": msg,
                    "status": "errored",
                }))
            }
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

    /// Choose which tier of model an agent runs on: 'small' or 'large'.
    ///
    /// The tiers are configuration, not model names:
    /// `pg_synapse.default_llm_profile_small` and `_main` say what small and
    /// large actually are, so swapping the model behind a tier is one setting
    /// rather than an edit to every agent that uses it.
    ///
    /// Has no effect on an agent whose `llm_profile_main` names a profile
    /// outright, because an explicit choice wins over a tier. Clear that
    /// column to let the tier decide.
    #[pg_extern(security_definer)]
    pub fn agent_set_model_tier(name: &str, tier: &str) {
        if !matches!(tier, "small" | "large") {
            pgrx::error!("invalid model tier '{tier}'; use 'small' or 'large'");
        }
        let args: Vec<DatumWithOid<'_>> = vec![
            DatumWithOid::from(tier.to_string()),
            DatumWithOid::from(name.to_string()),
        ];
        Spi::run_with_args(
            "UPDATE synapse.agents SET model_tier = $1, updated_at = now() WHERE name = $2",
            &args,
        )
        .unwrap_or_else(|e| pgrx::error!("agent_set_model_tier: {e}"));
        rebuild_kernel();
    }

    /// Set or clear the per-agent trace level. NULL inherits the global GUC.
    #[pg_extern(security_definer)]
    pub fn agent_set_trace_level(name: &str, level: Option<&str>) {
        if let Some(l) = level {
            let valid = ["off", "error", "info", "debug", "full"];
            if !valid.contains(&l) {
                pgrx::error!(
                    "invalid trace level '{}'; use one of: off, error, info, debug, full",
                    l
                );
            }
        }
        let args: Vec<DatumWithOid<'_>> = vec![
            match level {
                Some(l) => DatumWithOid::from(l.to_string()),
                None => DatumWithOid::null::<String>(),
            },
            DatumWithOid::from(name.to_string()),
        ];
        Spi::run_with_args(
            "UPDATE synapse.agents SET trace_level = $1, updated_at = now() WHERE name = $2",
            &args,
        )
        .unwrap();
        rebuild_kernel();
    }

    /// Delete executions (and their cascade-deleted messages/traces) older
    /// than the given interval. Returns the number of rows purged.
    #[pg_extern(security_definer)]
    pub fn purge_traces(older_than_days: i32, agent_filter: Option<&str>) -> i64 {
        let (sql, args): (String, Vec<DatumWithOid<'_>>) = match agent_filter {
            Some(a) => (
                format!(
                    "DELETE FROM synapse.executions WHERE started_at < now() - interval '{} days' AND agent_name = $1",
                    older_than_days
                ),
                vec![DatumWithOid::from(a.to_string())],
            ),
            None => (
                format!(
                    "DELETE FROM synapse.executions WHERE started_at < now() - interval '{} days'",
                    older_than_days
                ),
                vec![],
            ),
        };
        Spi::connect_mut(|client| {
            client
                .update(&sql, None, &args)
                .map(|t| t.len() as i64)
                .unwrap_or(0)
        })
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
        // With a key configured the value is stored as pgcrypto ciphertext and
        // flagged, so a database dump yields nothing usable on its own. Without
        // one it is stored as before: turning encryption on is an operator
        // decision, and a secret written before that decision stays readable
        // after it.
        match super::master_key() {
            Some(key) => {
                let enc_args: Vec<DatumWithOid<'_>> = vec![
                    DatumWithOid::from(name.to_string()),
                    DatumWithOid::from(value.to_string()),
                    DatumWithOid::from(key),
                ];
                Spi::run_with_args(
                    "INSERT INTO synapse.secrets (name, value, is_encrypted) \
                     VALUES ($1, armor(pgp_sym_encrypt($2, $3)), true) \
                     ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value, \
                       is_encrypted = true, updated_at = now()",
                    &enc_args,
                )
                .unwrap_or_else(|e| pgrx::error!("could not encrypt secret: {e}"));
            }
            None => {
                Spi::run_with_args(
                    "INSERT INTO synapse.secrets (name, value, is_encrypted) VALUES ($1,$2,false) ON CONFLICT (name) DO UPDATE SET value=EXCLUDED.value, is_encrypted=false, updated_at=now()",
                    &args,
                )
                .unwrap();
            }
        }
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

    /// Build the kernel cache if this backend has not built it yet.
    ///
    /// F2 slice 2a. Building the kernel reads agents, both profile tables and
    /// whichever secrets those profiles name. Once the entry points run as
    /// their caller, whoever triggers the build does so with their own rights,
    /// so the obvious move is to grant them the `synapse.config_*` functions
    /// that do those reads. That is a trap: `config_secrets(names text[])`
    /// granted to `synapse_user` is `SELECT any_secret_you_like`, demonstrated
    /// on a live database before this was written. A definer function is only
    /// a boundary while the caller cannot invoke it directly; the GRANT
    /// decides, not the attribute.
    ///
    /// This is the same reads behind a signature that is safe to hand out
    /// instead: no argument to steer it, no value to read back, and the work
    /// lands in a process-local cache. An attacker who calls it repeatedly
    /// achieves a rebuild they could already trigger by connecting again.
    ///
    /// Callers reach it through SPI rather than calling `kernel_handle()` in
    /// process, because crossing into a definer function is exactly what earns
    /// the owner's rights for the reads inside.
    /// Write a whole run's audit rows. Granted to caller roles, and useless
    /// without a token an entry point on this backend minted.
    ///
    /// The grant is not the authorisation: `synapse.record_run` underneath
    /// stays owner-only, and this refuses any call that cannot prove it
    /// belongs to a run in flight. Without that, granting the writer to
    /// `synapse_user` would just move the forgery one function along.
    ///
    /// Raises rather than returning false on a bad token. A caller who cannot
    /// prove the capability is not a caller whose write should quietly appear
    /// to have worked, and the audit trail is the wrong place to be lenient.
    #[pg_extern(security_definer)]
    pub fn audit_run(payload: JsonB, token: &str) {
        check_audit_token(token);
        Spi::run_with_args(
            "SELECT synapse.record_run($1)",
            &[DatumWithOid::from(payload)],
        )
        .unwrap_or_else(|e| pgrx::error!("audit_run: {e}"));
    }

    /// One executions row at a given status, under the same capability check
    /// as [`audit_run`].
    #[pg_extern(security_definer)]
    pub fn audit_status(payload: JsonB, token: &str) {
        check_audit_token(token);
        Spi::run_with_args(
            "SELECT synapse.record_status($1)",
            &[DatumWithOid::from(payload)],
        )
        .unwrap_or_else(|e| pgrx::error!("audit_status: {e}"));
    }

    /// Reject a call that cannot present a live capability token.
    ///
    /// The error deliberately says nothing about the token it was given: not
    /// whether it parsed, not how it differed, not how many are live. A
    /// direct caller learns only that they were refused.
    fn check_audit_token(token: &str) {
        let parsed = u128::from_str_radix(token, 16).ok();
        if !parsed.is_some_and(crate::audit_capability::is_live) {
            pgrx::error!(
                "audit writes are reachable only from a run in progress; \
                 this function cannot be called directly"
            );
        }
    }

    #[pg_extern(security_definer)]
    pub fn ensure_kernel() {
        super::warn_if_master_key_is_exposed();
        if let Err(e) = kernel_handle() {
            pgrx::error!("ensure_kernel: {e}");
        }
    }

    // ---- v0.1.1 N2.2: remaining SQL surface ----

    /// Return the capabilities of the named LLM profile's provider as JSONB.
    /// Lets operators verify what a profile supports before assigning it to
    /// an agent. Returns `{"error": "..."}` when the profile is not found.
    #[pg_extern(security_definer)]
    pub fn provider_capabilities(profile_name: &str) -> JsonB {
        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => {
                return JsonB(json!({ "error": e }));
            }
        };
        match kernel.provider_capabilities(profile_name) {
            Some(caps) => {
                // ProviderCapabilities derives Serialize, so this is safe.
                let v = serde_json::to_value(caps).unwrap_or(json!({}));
                JsonB(v)
            }
            None => JsonB(json!({
                "error": format!("LLM profile '{}' not registered", profile_name),
            })),
        }
    }

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
    #[pg_extern]
    pub fn tool_call(tool_name: &str, input: JsonB) -> JsonB {
        // `role`, not `current_user`: it reflects an explicit SET ROLE and
        // falls back to the login role when none is set, which is the identity
        // a grant or an RLS policy would be written against. This mattered
        // more when the function was SECURITY DEFINER and `current_user` was
        // the extension owner for everyone; now that it is INVOKER the two
        // usually agree, and `role` is still the one that is right when they
        // do not.
        let caller_role: Option<String> = Spi::get_one(
            "SELECT COALESCE(NULLIF(current_setting('role', true), 'none'), session_user::text)",
        )
        .ok()
        .flatten();

        // See `execute`: the kernel's config reads need the owner's rights.
        if let Err(e) = Spi::run("SELECT synapse.ensure_kernel()") {
            return JsonB(json!({ "error": e.to_string(), "status": "errored" }));
        }

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
    #[pg_extern]
    pub fn execute_async(agent_name: &str, input: &str) -> pgrx::Uuid {
        // `role`, not `current_user`: see `tool_call`.
        let caller_role: Option<String> = Spi::get_one(
            "SELECT COALESCE(NULLIF(current_setting('role', true), 'none'), session_user::text)",
        )
        .ok()
        .flatten();

        // See `execute`: the kernel's config reads need the owner's rights.
        if let Err(e) = Spi::run("SELECT synapse.ensure_kernel()") {
            pgrx::error!("execute_async: {e}");
        }

        // Pre-insert a 'queued' row keyed by a fresh id so a poller can see
        // the execution exists even if the run below fails hard.
        let grant = AuditGrant::mint();

        let queued_id = uuid::Uuid::new_v4();
        let _ = super::record_status(
            &queued_id.to_string(),
            agent_name,
            Some(input),
            None,
            "queued",
            caller_role.as_deref(),
            &grant,
        );

        let kernel = match kernel_handle() {
            Ok(k) => k,
            Err(e) => {
                let _ = super::record_status(
                    &queued_id.to_string(),
                    agent_name,
                    Some(input),
                    Some(&format!("kernel error: {e}")),
                    "errored",
                    caller_role.as_deref(),
                    &grant,
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
                // The kernel minted its own execution_id for the messages,
                // so the placeholder written above is superseded rather than
                // updated. It is dropped inside the same statement that writes
                // the real row, through the shared logger, so the sync and
                // async paths produce identical audit rows.
                let tl = resolve_trace_level(agent_name);
                // Not `let _ =`: a failure to record a run used to be silent,
                // which is how a timed-out run went missing without anyone
                // noticing. It still must not fail the caller's query, so it
                // is reported as a warning rather than raised.
                if let Err(e) = log_execution(
                    &o,
                    agent_name,
                    input,
                    caller_role.as_deref(),
                    tl,
                    Some(&queued_id.to_string()),
                    &grant,
                ) {
                    pgrx::warning!("could not record execution for agent {agent_name}: {e}");
                }
                let real_id = o
                    .messages
                    .first()
                    .map(|m| m.execution_id)
                    .unwrap_or(queued_id);
                pgrx::Uuid::from_bytes(*real_id.as_bytes())
            }
            Err(e) => {
                let _ = super::record_status(
                    &queued_id.to_string(),
                    agent_name,
                    Some(input),
                    Some(&e.to_string()),
                    "errored",
                    caller_role.as_deref(),
                    &grant,
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
        // Respect the concurrency ceiling before claiming anything. An agent
        // run holds a backend for the length of an LLM call, so twenty
        // schedules coming due together would otherwise try to run twenty at
        // once and take the connection pool with them. Claiming fewer makes a
        // busy morning slow instead of an outage; the rest stay queued and are
        // picked up on the next drain.
        let cap = crate::schema_guc::MAX_CONCURRENT_RUNS.get();
        let running: i64 =
            Spi::get_one("SELECT count(*) FROM synapse.agent_queue WHERE status = 'running'")
                .ok()
                .flatten()
                .unwrap_or(0);
        let headroom = (cap as i64 - running).max(0);
        if headroom == 0 {
            return 0;
        }
        let max_jobs = max_jobs.min(headroom as i32);
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
                     SELECT job_id::text, agent, input FROM claimed",
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

            // Decide the queue's terminal status from the envelope. See
            // `queue_status_for`: only "completed" is a success.
            let (new_status, error_val) = crate::queue_status_for(&result_jsonb.0);
            let result_val = Some(result_jsonb.0.clone());

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
    // SECURITY INVOKER on purpose, unlike the rest of this surface. Attaching a
    // trigger is DDL against the caller's own table, so it should require the
    // caller's own privileges: a table owner may instrument their table, and
    // nobody may instrument a table they do not own. Running it as the
    // extension owner would either need that role to hold TRIGGER on every
    // table in the database, or fail, and the first of those is a far worse
    // trade than the second. EXECUTE is still restricted to synapse_admin in
    // grants.sql.
    #[pg_extern]
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
    -- NULLIF because a run that did not complete has output '', not NULL, so
    -- a plain COALESCE stopped there and raised "rejected: " with no reason.
    -- The person reading this is a DBA whose write just rolled back.
    _reason := COALESCE(
      _res->>'error',
      NULLIF(_out, ''),
      CASE WHEN _status = 'timed_out'
           THEN 'agent exceeded the inline budget, see pg_synapse.inline_timeout_ms'
           ELSE 'agent did not complete, status ' || COALESCE(_status, 'unknown') END
    );
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
    // SECURITY INVOKER for the same reason as attach_agent_trigger: dropping a
    // trigger is DDL on the caller's table.
    #[pg_extern]
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
