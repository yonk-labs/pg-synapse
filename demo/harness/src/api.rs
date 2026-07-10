//! JSON HTTP endpoints wrapping the `synapse.*` SQL surface.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_postgres::types::ToSql;
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

/// The friendly message shown whenever an agent would run without a usable
/// LLM endpoint. Keeps the raw kernel "provider ... not registered" error off
/// the presenter's screen.
pub const NO_ENDPOINT_MSG: &str =
    "No LLM endpoint configured - connect and Save your endpoint first \
     (or the agent references a missing profile).";

/// Guard: the target agent must reference an `llm_profile_main` that exists in
/// `synapse.llm_profiles`. Returns BadRequest with a friendly message when the
/// endpoint is missing. If the agent itself is absent, we let the normal
/// execute path report that (it is a separate, less common failure).
async fn require_agent_endpoint(
    client: &tokio_postgres::Client,
    agent: &str,
) -> Result<(), HarnessError> {
    let row = client
        .query_opt(
            "SELECT EXISTS (SELECT 1 FROM synapse.llm_profiles p WHERE p.name = a.llm_profile_main) \
             FROM synapse.agents a WHERE a.name = $1",
            &[&agent],
        )
        .await?;
    if let Some(row) = row {
        let has_profile: bool = row.get(0);
        if !has_profile {
            return Err(HarnessError::BadRequest(NO_ENDPOINT_MSG.to_owned()));
        }
    }
    Ok(())
}

/// Translate a raw kernel/provider "not registered" error into the friendly
/// endpoint message; pass anything else through unchanged.
pub fn friendly_agent_error(raw: &str) -> String {
    if raw.to_lowercase().contains("not registered") {
        NO_ENDPOINT_MSG.to_owned()
    } else {
        raw.to_owned()
    }
}

pub async fn execute(
    State(state): State<AppState>,
    Json(req): Json<ExecuteReq>,
) -> Result<Json<Value>, HarnessError> {
    if req.agent.trim().is_empty() {
        return Err(HarnessError::BadRequest("agent is required".to_owned()));
    }
    let client = db::connect(&state.db_url).await?;
    require_agent_endpoint(&client, &req.agent).await?;
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
            // Surface the rollback reason (the agent's rejection); translate a
            // missing-endpoint kernel error into the friendly message.
            let reason = e
                .as_db_error()
                .map(|d| d.message().to_owned())
                .unwrap_or_else(|| e.to_string());
            Json(json!({"ok": true, "committed": false, "rolled_back": true,
                "reason": friendly_agent_error(&reason)}))
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

// ---- schema browser (catalog-validated table viewer + editor) ----
//
// Every identifier (schema, table, column) is validated against the live
// catalog before use and then quoted; every value is bound as a $ parameter
// and cast through the column's real type. Nothing user-supplied is ever
// string-interpolated as a value, and a table absent from the catalog is
// rejected rather than executed.

/// Double-quote an identifier, doubling any embedded quotes. Only ever called
/// on identifiers already confirmed to exist in the catalog.
fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// JSON value to the text we bind as a `$n` parameter: null stays SQL NULL,
/// strings pass through raw, everything else uses its compact JSON form.
fn as_sql_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// True when `schema.table` is a user table or view (never a system catalog).
async fn table_in_catalog(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<bool, HarnessError> {
    let row = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2 \
             AND table_schema NOT IN ('pg_catalog', 'information_schema'))",
            &[&schema, &table],
        )
        .await?;
    Ok(row.get(0))
}

/// The column's castable type string (e.g. `numeric(12,2)`, `text`) or None if
/// the column does not exist. Doubles as column-existence validation. The
/// returned string comes from `format_type`, not user input.
async fn column_type(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
    col: &str,
) -> Result<Option<String>, HarnessError> {
    let row = client
        .query_opt(
            "SELECT format_type(a.atttypid, a.atttypmod) \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND a.attname = $3 \
             AND a.attnum > 0 AND NOT a.attisdropped",
            &[&schema, &table, &col],
        )
        .await?;
    Ok(row.map(|r| r.get::<_, String>(0)))
}

async fn require_table(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<(), HarnessError> {
    if table_in_catalog(client, schema, table).await? {
        Ok(())
    } else {
        Err(HarnessError::BadRequest(format!(
            "table \"{schema}.{table}\" is not a browsable table in this database"
        )))
    }
}

pub async fn schema_tables(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let rows = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(x)::text FROM (SELECT table_schema AS schema, table_name AS table \
         FROM information_schema.tables \
         WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
         AND table_type IN ('BASE TABLE', 'VIEW') \
         ORDER BY table_schema, table_name) x",
        &[],
    )
    .await?;
    Ok(Json(json!({"ok": true, "tables": rows})))
}

#[derive(Deserialize)]
pub struct SchemaRef {
    pub schema: String,
    pub table: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn schema_columns(
    State(state): State<AppState>,
    Query(q): Query<SchemaRef>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    require_table(&client, &q.schema, &q.table).await?;
    let rows = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(x)::text FROM ( \
           SELECT c.column_name AS column, c.data_type AS type, \
             (c.is_nullable = 'YES') AS nullable, c.column_default AS default, \
             COALESCE(pk.is_pk, false) AS pk \
           FROM information_schema.columns c \
           LEFT JOIN ( \
             SELECT kcu.column_name, true AS is_pk \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
               AND tc.table_schema = $1 AND tc.table_name = $2 \
           ) pk ON pk.column_name = c.column_name \
           WHERE c.table_schema = $1 AND c.table_name = $2 \
           ORDER BY c.ordinal_position) x",
        &[&q.schema, &q.table],
    )
    .await?;
    Ok(Json(json!({"ok": true, "columns": rows})))
}

pub async fn schema_rows(
    State(state): State<AppState>,
    Query(q): Query<SchemaRef>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    require_table(&client, &q.schema, &q.table).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    let sql = format!(
        "SELECT to_jsonb(x)::text FROM (SELECT * FROM {}.{} LIMIT $1) x",
        quote_ident(&q.schema),
        quote_ident(&q.table),
    );
    let rows = db::jsonb_rows(&client, &sql, &[&limit]).await?;
    Ok(Json(json!({"ok": true, "rows": rows, "limit": limit})))
}

#[derive(Deserialize)]
pub struct SchemaUpdateReq {
    pub schema: String,
    pub table: String,
    pub pk_col: String,
    pub pk_val: Value,
    pub col: String,
    pub val: Value,
}

pub async fn schema_update(
    State(state): State<AppState>,
    Json(req): Json<SchemaUpdateReq>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    require_table(&client, &req.schema, &req.table).await?;
    let col_type = column_type(&client, &req.schema, &req.table, &req.col)
        .await?
        .ok_or_else(|| HarnessError::BadRequest(format!("unknown column '{}'", req.col)))?;
    let pk_type = column_type(&client, &req.schema, &req.table, &req.pk_col)
        .await?
        .ok_or_else(|| HarnessError::BadRequest(format!("unknown pk column '{}'", req.pk_col)))?;
    // Identifiers are catalog-validated and quoted; type strings come from
    // format_type; only the two $ values are user data.
    let sql = format!(
        "UPDATE {}.{} SET {} = $1::text::{} WHERE {} = $2::text::{}",
        quote_ident(&req.schema),
        quote_ident(&req.table),
        quote_ident(&req.col),
        col_type,
        quote_ident(&req.pk_col),
        pk_type,
    );
    let val = as_sql_text(&req.val);
    let pk_val = as_sql_text(&req.pk_val);
    let n = client.execute(&sql, &[&val, &pk_val]).await?;
    Ok(Json(json!({"ok": true, "updated": n})))
}

#[derive(Deserialize)]
pub struct SchemaInsertReq {
    pub schema: String,
    pub table: String,
    pub values: serde_json::Map<String, Value>,
}

pub async fn schema_insert(
    State(state): State<AppState>,
    Json(req): Json<SchemaInsertReq>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    require_table(&client, &req.schema, &req.table).await?;
    if req.values.is_empty() {
        return Err(HarnessError::BadRequest(
            "insert needs at least one column value".to_owned(),
        ));
    }
    let mut cols = Vec::new();
    let mut placeholders = Vec::new();
    let mut vals: Vec<Option<String>> = Vec::new();
    for (col, v) in &req.values {
        let ty = column_type(&client, &req.schema, &req.table, col)
            .await?
            .ok_or_else(|| HarnessError::BadRequest(format!("unknown column '{col}'")))?;
        vals.push(as_sql_text(v));
        placeholders.push(format!("${}::text::{}", vals.len(), ty));
        cols.push(quote_ident(col));
    }
    let sql = format!(
        "INSERT INTO {}.{} ({}) VALUES ({})",
        quote_ident(&req.schema),
        quote_ident(&req.table),
        cols.join(", "),
        placeholders.join(", "),
    );
    let params: Vec<&(dyn ToSql + Sync)> = vals.iter().map(|v| v as &(dyn ToSql + Sync)).collect();
    let n = client.execute(&sql, &params).await?;
    Ok(Json(json!({"ok": true, "inserted": n})))
}

// ---- SQL console ----
//
// Arbitrary SQL against the local demo database is the point (the audience
// wants to call synapse.execute() like any other function), so this is not
// sandboxed to read-only. It runs on its own short-lived connection with a
// generous timeout so a slow LLM-backed agent call cannot block other
// requests, and every failure comes back as a clean JSON error.

#[derive(Deserialize)]
pub struct SqlReq {
    pub sql: String,
}

const SQL_TIMEOUT_SECS: u64 = 180;
const SQL_ROW_CAP: usize = 1000;

pub async fn run_sql(State(state): State<AppState>, Json(req): Json<SqlReq>) -> Json<Value> {
    if req.sql.trim().is_empty() {
        return Json(json!({"ok": false, "error": "empty statement"}));
    }
    let fut = async {
        let client = db::connect(&state.db_url).await?;
        let msgs = client.simple_query(&req.sql).await?;
        Ok::<Vec<tokio_postgres::SimpleQueryMessage>, HarnessError>(msgs)
    };
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(SQL_TIMEOUT_SECS), fut).await;

    let msgs = match outcome {
        Err(_) => {
            return Json(json!({
                "ok": false,
                "error": format!("statement timed out after {SQL_TIMEOUT_SECS}s"),
            }));
        }
        Ok(Err(HarnessError::Db(e))) => {
            let msg = e
                .as_db_error()
                .map(|d| d.message().to_owned())
                .unwrap_or_else(|| e.to_string());
            return Json(json!({"ok": false, "error": msg}));
        }
        Ok(Err(e)) => return Json(json!({"ok": false, "error": e.to_string()})),
        Ok(Ok(msgs)) => msgs,
    };

    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut rows_affected: u64 = 0;
    let mut truncated = false;
    for msg in msgs {
        match msg {
            tokio_postgres::SimpleQueryMessage::Row(row) => {
                if columns.is_empty() {
                    columns = row.columns().iter().map(|c| c.name().to_owned()).collect();
                }
                if rows.len() < SQL_ROW_CAP {
                    let cells = (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect();
                    rows.push(cells);
                } else {
                    truncated = true;
                }
            }
            tokio_postgres::SimpleQueryMessage::CommandComplete(n) => {
                rows_affected = n;
            }
            _ => {}
        }
    }

    if !columns.is_empty() {
        Json(json!({
            "ok": true,
            "columns": columns,
            "rows": rows,
            "truncated": truncated,
        }))
    } else {
        Json(json!({
            "ok": true,
            "command": "statement executed",
            "rows_affected": rows_affected,
            "notices": [],
        }))
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
