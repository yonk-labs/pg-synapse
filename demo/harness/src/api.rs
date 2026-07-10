//! JSON HTTP endpoints wrapping the `synapse.*` SQL surface.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db;
use crate::error::HarnessError;
use crate::runs;
use crate::scenarios;
use crate::AppState;

/// Tools compiled into the demo extension image. `synapse.tool_list()` only
/// reflects rows registered via `synapse.tool_register`, so the UI needs this
/// baked list of plugin-provided tools.
const BUILTIN_TOOLS: &[(&str, &str)] = &[
    ("sql_query", "Run a read-only SQL query with $n params"),
    ("sql_exec", "Run a writing SQL statement with $n params"),
    ("http_get", "HTTP GET a URL"),
    ("http_post", "HTTP POST to a URL"),
    ("http_head", "HTTP HEAD a URL"),
    ("calculator", "Evaluate an arithmetic expression"),
    ("get_current_time", "Read the current time"),
    ("call_agent", "Delegate to another registered agent"),
];

const EXECUTORS: &[&str] = &["conversation", "react", "reflection"];

/// Whitelisted table views for the UI's live panels. Each query returns one
/// `to_jsonb(...)::text` column per row.
const TABLE_QUERIES: &[(&str, &str)] = &[
    (
        "support_tickets",
        "SELECT to_jsonb(x)::text FROM (SELECT id, subject, category, priority, escalated \
         FROM support.tickets ORDER BY id) x",
    ),
    (
        "tickets",
        "SELECT to_jsonb(x)::text FROM (SELECT id, subject, category, priority \
         FROM demo.tickets ORDER BY id) x",
    ),
    (
        "orders",
        "SELECT to_jsonb(x)::text FROM (SELECT id, customer, amount, status \
         FROM demo.orders ORDER BY id) x",
    ),
    (
        "queue",
        "SELECT to_jsonb(x)::text FROM (SELECT job_id, agent, left(input, 90) AS input, \
         status, error, source, enqueued_at, finished_at \
         FROM synapse.agent_queue ORDER BY enqueued_at DESC LIMIT 20) x",
    ),
    (
        "executions",
        "SELECT to_jsonb(x)::text FROM (SELECT execution_id, agent_name, left(input, 60) AS input, \
         status, tokens_in, tokens_out, cost_usd, duration_ms, started_at \
         FROM synapse.executions ORDER BY started_at DESC LIMIT 20) x",
    ),
    (
        "signals",
        "SELECT to_jsonb(x)::text FROM (SELECT id, signal, left(detail, 90) AS detail, resolved \
         FROM dba.health_signals ORDER BY id) x",
    ),
    (
        "recommendations",
        "SELECT to_jsonb(x)::text FROM (SELECT id, signal_id, severity, \
         left(recommendation, 80) AS recommendation, requires_human \
         FROM dba.recommendations ORDER BY id) x",
    ),
    (
        "raw_contacts",
        "SELECT to_jsonb(x)::text FROM (SELECT id, left(note, 110) AS note \
         FROM etl.raw_contacts ORDER BY id) x",
    ),
    (
        "contacts",
        "SELECT to_jsonb(x)::text FROM (SELECT id, raw_id, name, company, email, country_code, \
         intent FROM etl.contacts ORDER BY id) x",
    ),
];

/// Whitelisted read-only probes for the UI (EXPLAIN views and per-scenario
/// end-state assertions). Run over the simple-query protocol; each returns
/// text lines.
const PROBE_QUERIES: &[(&str, &str)] = &[
    (
        "explain_orders",
        "EXPLAIN (ANALYZE, BUFFERS) SELECT count(*), sum(amount) FROM perf.orders \
         WHERE customer_id = 4242",
    ),
    (
        "perf_indexes",
        "SELECT indexname || ' : ' || indexdef FROM pg_indexes \
         WHERE schemaname = 'perf' AND tablename = 'orders' ORDER BY indexname",
    ),
    (
        "assert_index_tuner",
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'perf' \
           AND tablename = 'orders' AND indexdef ILIKE '%customer_id%') \
           THEN 'PASS: index on perf.orders(customer_id) exists' \
           ELSE 'FAIL: no index on perf.orders(customer_id)' END \
         UNION ALL \
         SELECT CASE WHEN EXISTS (SELECT 1 FROM perf.explain_query( \
             'SELECT count(*), sum(amount) FROM perf.orders WHERE customer_id = 4242') AS line \
             WHERE line ILIKE '%index%scan%') \
           THEN 'PASS: the planner now uses an index scan' \
           ELSE 'FAIL: the planner still seq-scans' END",
    ),
    (
        "assert_dba",
        "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM dba.health_signals WHERE resolved = false) \
           THEN 'PASS: all signals resolved' ELSE 'FAIL: unresolved signals remain' END \
         UNION ALL \
         SELECT CASE WHEN (SELECT count(*) FROM dba.recommendations WHERE requires_human) >= 3 \
           THEN 'PASS: tickets filed for the non-transaction-safe fixes' \
           ELSE 'FAIL: expected at least 3 human tickets' END \
         UNION ALL \
         SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'dba' \
           AND tablename = 'audit_log' AND indexdef ILIKE '%actor_id%') \
           THEN 'PASS: dba.audit_log(actor_id) index auto-created' \
           ELSE 'FAIL: dba.audit_log(actor_id) index missing' END",
    ),
    (
        "assert_etl",
        "SELECT CASE WHEN (SELECT count(*) FROM etl.raw_contacts) = \
             (SELECT count(*) FROM etl.contacts) \
           THEN 'PASS: every raw row has a clean row (' || \
             (SELECT count(*) FROM etl.contacts)::text || ' of ' || \
             (SELECT count(*) FROM etl.raw_contacts)::text || ')' \
           ELSE 'FAIL: ' || ((SELECT count(*) FROM etl.raw_contacts) - \
             (SELECT count(*) FROM etl.contacts))::text || ' raw row(s) unprocessed' END",
    ),
    (
        "assert_triage",
        "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM support.tickets WHERE category IS NULL) \
           THEN 'PASS: every ticket categorized' \
           ELSE 'FAIL: ' || (SELECT count(*) FROM support.tickets WHERE category IS NULL)::text \
             || ' ticket(s) still uncategorized' END",
    ),
    (
        "assert_bouncer",
        "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM demo.orders WHERE amount <= 0) \
           THEN 'PASS: no bad order committed' \
           ELSE 'FAIL: a non-positive order got through the gate' END",
    ),
];

// ---- bootstrap ----

pub async fn bootstrap(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let agents = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(a)::text FROM (SELECT name, system_prompt, executor_name, \
         llm_profile_main, tools, max_iterations, timeout_ms, cost_cap_usd::float8, trace_level \
         FROM synapse.agents ORDER BY name) a",
        &[],
    )
    .await?;
    let profiles = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(p)::text FROM (SELECT name, provider, model, base_url, \
         (api_key_secret IS NOT NULL) AS has_api_key FROM synapse.llm_profiles ORDER BY name) p",
        &[],
    )
    .await?;
    let version: String = client
        .query_one("SELECT synapse.version()", &[])
        .await?
        .get(0);

    let tools: Vec<Value> = BUILTIN_TOOLS
        .iter()
        .map(|(n, d)| json!({"name": n, "description": d}))
        .collect();
    let scenario_meta: Vec<Value> = scenarios::SCENARIOS
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
        .collect();

    Ok(Json(json!({
        "ok": true,
        "version": version,
        "agents": agents,
        "profiles": profiles,
        "tools": tools,
        "executors": EXECUTORS,
        "scenarios": scenario_meta,
        "default_llm": {
            "base_url": state.default_llm_base_url,
            "model": state.default_llm_model,
        },
    })))
}

// ---- LLM profile ----

#[derive(Deserialize)]
pub struct ProfileReq {
    #[serde(default = "default_profile_name")]
    pub name: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

fn default_profile_name() -> String {
    "vllm-default".to_owned()
}

pub async fn profile_set(
    State(state): State<AppState>,
    Json(req): Json<ProfileReq>,
) -> Result<Json<Value>, HarnessError> {
    if req.base_url.trim().is_empty() || req.model.trim().is_empty() {
        return Err(HarnessError::BadRequest(
            "base_url and model are required".to_owned(),
        ));
    }
    let client = db::connect(&state.db_url).await?;
    let secret_name = if req.api_key.trim().is_empty() {
        None
    } else {
        let name = format!("{}_api_key", req.name);
        client
            .execute("SELECT synapse.secret_set($1, $2)", &[&name, &req.api_key])
            .await?;
        Some(name)
    };
    client
        .execute(
            "SELECT synapse.llm_profile_set($1, 'openai', $2, $3, $4, '{}'::jsonb)",
            &[&req.name, &req.model, &req.base_url, &secret_name],
        )
        .await?;
    Ok(Json(json!({"ok": true, "profile": req.name})))
}

#[derive(Deserialize)]
pub struct ProfileTestReq {
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
}

/// Probe the user-supplied OpenAI-compatible endpoint directly from the
/// harness (GET {base_url}/models). This validates reachability and auth
/// before anything is persisted.
pub async fn profile_test(Json(req): Json<ProfileTestReq>) -> Result<Json<Value>, HarnessError> {
    let url = format!("{}/models", req.base_url.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()?;
    let mut r = http.get(&url);
    if !req.api_key.trim().is_empty() {
        r = r.bearer_auth(req.api_key.trim());
    }
    let resp = r.send().await?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let models: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "ok": (200..300).contains(&(status as i32)),
        "status": status,
        "models": models,
    })))
}

// ---- agents ----

#[derive(Deserialize)]
pub struct AgentReq {
    pub name: String,
    pub system_prompt: String,
    #[serde(default = "default_executor")]
    pub executor: String,
    #[serde(default = "default_profile_name")]
    pub llm_profile: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: i32,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: i64,
    /// None or 0 clears the cap.
    #[serde(default)]
    pub cost_cap_usd: Option<f64>,
}

fn default_executor() -> String {
    "conversation".to_owned()
}
fn default_max_iterations() -> i32 {
    10
}
fn default_timeout_ms() -> i64 {
    60_000
}

pub async fn agent_set(
    State(state): State<AppState>,
    Json(req): Json<AgentReq>,
) -> Result<Json<Value>, HarnessError> {
    if req.name.trim().is_empty() || req.system_prompt.trim().is_empty() {
        return Err(HarnessError::BadRequest(
            "name and system_prompt are required".to_owned(),
        ));
    }
    let client = db::connect(&state.db_url).await?;
    client
        .execute(
            "SELECT synapse.agent_create($1, $2, $3, $4, $5, $6, $7)",
            &[
                &req.name,
                &req.system_prompt,
                &req.executor,
                &req.llm_profile,
                &req.tools,
                &req.max_iterations,
                &req.timeout_ms,
            ],
        )
        .await?;
    // agent_create has no cost-cap parameter; the cap lives on the row.
    let cap = req.cost_cap_usd.filter(|c| *c > 0.0);
    client
        .execute(
            "UPDATE synapse.agents SET cost_cap_usd = ($2::float8)::numeric(12,6) WHERE name = $1",
            &[&req.name, &cap],
        )
        .await?;
    // Persist full message + event traces for the UI trace view, and rebuild
    // the kernel cache so the cap UPDATE is picked up.
    client
        .execute(
            "SELECT synapse.agent_set_trace_level($1, 'debug')",
            &[&req.name],
        )
        .await?;
    Ok(Json(json!({"ok": true, "agent": req.name})))
}

#[derive(Deserialize)]
pub struct AgentDropReq {
    pub name: String,
}

pub async fn agent_drop(
    State(state): State<AppState>,
    Json(req): Json<AgentDropReq>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    client
        .execute("SELECT synapse.agent_drop($1)", &[&req.name])
        .await?;
    Ok(Json(json!({"ok": true})))
}

// ---- execute + runs ----

#[derive(Deserialize)]
pub struct ExecuteReq {
    pub agent: String,
    pub input: String,
}

pub async fn execute(
    State(state): State<AppState>,
    Json(req): Json<ExecuteReq>,
) -> Result<Json<Value>, HarnessError> {
    if req.agent.trim().is_empty() {
        return Err(HarnessError::BadRequest("agent is required".to_owned()));
    }
    let run_id = runs::start_run(
        state.runs.clone(),
        state.db_url.clone(),
        req.agent,
        req.input,
    );
    Ok(Json(json!({"ok": true, "run_id": run_id})))
}

pub async fn run_status(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<Value>, HarnessError> {
    let snapshot = state
        .runs
        .lock()
        .ok()
        .and_then(|map| map.get(&run_id).cloned())
        .ok_or_else(|| HarnessError::NotFound(format!("run {run_id}")))?;
    Ok(Json(json!({"ok": true, "run": snapshot})))
}

pub async fn run_cancel(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<Value>, HarnessError> {
    let pid = state
        .runs
        .lock()
        .ok()
        .and_then(|map| map.get(&run_id).and_then(|r| r.backend_pid))
        .ok_or_else(|| HarnessError::NotFound(format!("run {run_id} has no backend pid")))?;
    let client = db::connect(&state.db_url).await?;
    let row = client
        .query_one("SELECT pg_cancel_backend($1)", &[&pid])
        .await?;
    let signalled: bool = row.get(0);
    Ok(Json(
        json!({"ok": true, "signalled": signalled, "pid": pid}),
    ))
}

// ---- reactive triggers + event demo ----

#[derive(Deserialize)]
pub struct TriggerAttachReq {
    pub table: String,
    pub agent: String,
    #[serde(default = "default_trigger_mode")]
    pub mode: String,
}

fn default_trigger_mode() -> String {
    "queue".to_owned()
}

/// The trigger demo only ever targets the two seeded demo tables.
fn check_demo_table(table: &str) -> Result<(), HarnessError> {
    if table == "demo.tickets" || table == "demo.orders" {
        Ok(())
    } else {
        Err(HarnessError::BadRequest(format!(
            "table '{table}' is not one of the demo tables (demo.tickets, demo.orders)"
        )))
    }
}

pub async fn trigger_attach(
    State(state): State<AppState>,
    Json(req): Json<TriggerAttachReq>,
) -> Result<Json<Value>, HarnessError> {
    check_demo_table(&req.table)?;
    if req.mode != "queue" && req.mode != "inline" {
        return Err(HarnessError::BadRequest(
            "mode must be 'queue' or 'inline'".to_owned(),
        ));
    }
    let client = db::connect(&state.db_url).await?;
    client
        .execute(
            "SELECT synapse.attach_agent_trigger($1, $2, $3, 'INSERT', NULL, 'NEW::text')",
            &[&req.table, &req.agent, &req.mode],
        )
        .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct TriggerDetachReq {
    pub table: String,
}

pub async fn trigger_detach(
    State(state): State<AppState>,
    Json(req): Json<TriggerDetachReq>,
) -> Result<Json<Value>, HarnessError> {
    check_demo_table(&req.table)?;
    let client = db::connect(&state.db_url).await?;
    client
        .execute("SELECT synapse.detach_agent_trigger($1)", &[&req.table])
        .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct InsertTicketReq {
    pub subject: String,
    pub body: String,
}

pub async fn insert_ticket(
    State(state): State<AppState>,
    Json(req): Json<InsertTicketReq>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let row = client
        .query_one(
            "INSERT INTO demo.tickets (subject, body) VALUES ($1, $2) RETURNING id",
            &[&req.subject, &req.body],
        )
        .await?;
    let id: i32 = row.get(0);
    Ok(Json(json!({"ok": true, "id": id})))
}

#[derive(Deserialize)]
pub struct InsertOrderReq {
    pub customer: String,
    pub amount: f64,
}

/// The showstopper: an INSERT into the inline-gated table. When the policy
/// agent rejects the row, the transaction rolls back and this returns the
/// Postgres error carrying the agent's reason.
pub async fn insert_order(
    State(state): State<AppState>,
    Json(req): Json<InsertOrderReq>,
) -> Json<Value> {
    let result = async {
        let client = db::connect(&state.db_url).await?;
        let row = client
            .query_one(
                "INSERT INTO demo.orders (customer, amount) \
                 VALUES ($1, ($2::float8)::numeric(12,2)) RETURNING id",
                &[&req.customer, &req.amount],
            )
            .await?;
        let id: i32 = row.get(0);
        Ok::<i32, HarnessError>(id)
    }
    .await;

    match result {
        Ok(id) => Json(json!({"ok": true, "committed": true, "id": id})),
        Err(HarnessError::Db(e)) => {
            // Surface the rollback reason (the agent's rejection) verbatim.
            let reason = e
                .as_db_error()
                .map(|d| d.message().to_owned())
                .unwrap_or_else(|| e.to_string());
            Json(json!({"ok": true, "committed": false, "rolled_back": true, "reason": reason}))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn drain_queue(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let row = client
        .query_one("SELECT synapse.drain_queue(10)", &[])
        .await?;
    let processed: i32 = row.get(0);
    Ok(Json(json!({"ok": true, "processed": processed})))
}

// ---- table views ----

pub async fn table_view(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let sql = TABLE_QUERIES
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, q)| *q)
        .ok_or_else(|| HarnessError::NotFound(format!("table view '{name}'")))?;
    let client = db::connect(&state.db_url).await?;
    match db::jsonb_rows(&client, sql, &[]).await {
        Ok(rows) => Ok(Json(json!({"ok": true, "rows": rows}))),
        // Scenario tables may not exist until the scenario is loaded.
        Err(HarnessError::Db(e)) => Ok(Json(json!({
            "ok": false,
            "rows": [],
            "error": e.as_db_error().map(|d| d.message().to_owned()).unwrap_or_else(|| e.to_string()),
        }))),
        Err(e) => Err(e),
    }
}

// ---- probes ----

/// Run a whitelisted probe (EXPLAIN or end-state assertion) and return its
/// output as text lines. Missing scenario objects come back as an error line
/// rather than a 500, so the UI can say "load the scenario first".
pub async fn probe(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let sql = PROBE_QUERIES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, q)| *q)
        .ok_or_else(|| HarnessError::NotFound(format!("probe '{key}'")))?;
    let client = db::connect(&state.db_url).await?;
    match client.simple_query(sql).await {
        Ok(messages) => {
            let mut lines = Vec::new();
            for msg in messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    lines.push(row.get(0).unwrap_or("").to_owned());
                }
            }
            Ok(Json(json!({"ok": true, "lines": lines})))
        }
        Err(e) => {
            let reason = e
                .as_db_error()
                .map(|d| d.message().to_owned())
                .unwrap_or_else(|| e.to_string());
            Ok(Json(json!({
                "ok": false,
                "lines": [],
                "error": format!("{reason} (is the scenario loaded?)"),
            })))
        }
    }
}

// ---- executions / messages lookup (history) ----

pub async fn execution_detail(
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let id = execution_id.to_string();
    let execution = db::jsonb_one(
        &client,
        "SELECT COALESCE((SELECT to_jsonb(e) FROM (SELECT execution_id, agent_name, input, output, \
         status, tokens_in, tokens_out, cost_usd, duration_ms, started_at, finished_at \
         FROM synapse.executions WHERE execution_id = ($1::text)::uuid) e), 'null'::jsonb)::text",
        &[&id],
    )
    .await?;
    let messages = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(m)::text FROM ( \
           SELECT seq, role, content, tool_call_id, tool_name, tool_input, tool_output \
           FROM synapse.messages WHERE execution_id = ($1::text)::uuid ORDER BY seq) m",
        &[&id],
    )
    .await?;
    let traces = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(t)::text FROM ( \
           SELECT seq, event, payload FROM synapse.traces \
           WHERE execution_id = ($1::text)::uuid ORDER BY seq) t",
        &[&id],
    )
    .await?;
    Ok(Json(json!({
        "ok": true,
        "execution": execution,
        "messages": messages,
        "traces": traces,
    })))
}

// ---- scenarios ----

pub async fn scenario_load(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let scenario =
        scenarios::find(&id).ok_or_else(|| HarnessError::NotFound(format!("scenario '{id}'")))?;
    let client = db::connect(&state.db_url).await?;
    client.batch_execute(scenario.sql).await?;
    Ok(Json(json!({
        "ok": true,
        "scenario": serde_json::to_value(scenario).unwrap_or(Value::Null),
    })))
}
