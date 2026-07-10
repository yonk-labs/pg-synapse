//! In-memory registry of agent runs.
//!
//! Each run gets its own Postgres connection so the backend PID can be
//! cancelled independently, and so a long agent loop never blocks other
//! harness queries. `synapse.execute()` is synchronous: the SELECT returns
//! when the loop finishes, and the messages / traces land in
//! `synapse.messages` / `synapse.traces` at that point (the runtime persists
//! after completion, not mid-run). The UI short-polls `/api/run/:id` for a
//! live feel and renders the trace when the run lands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use uuid::Uuid;

use crate::db;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Starting,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Serialize)]
pub struct RunState {
    pub run_id: Uuid,
    pub agent: String,
    pub input: String,
    pub phase: RunPhase,
    pub backend_pid: Option<i32>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    /// The JSON envelope returned by `synapse.execute`.
    pub envelope: Option<serde_json::Value>,
    /// Rows from `synapse.messages` for this execution.
    pub messages: Vec<serde_json::Value>,
    /// Rows from `synapse.traces` for this execution.
    pub traces: Vec<serde_json::Value>,
    /// Harness-level failure (connection refused, statement cancelled, ...).
    pub error: Option<String>,
    /// True when the failure was a client-issued cancel.
    pub cancelled: bool,
}

pub type RunRegistry = Arc<Mutex<HashMap<Uuid, RunState>>>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn update<F: FnOnce(&mut RunState)>(registry: &RunRegistry, run_id: Uuid, f: F) {
    if let Ok(mut map) = registry.lock() {
        if let Some(state) = map.get_mut(&run_id) {
            f(state);
        }
    }
}

/// Register a run and spawn the task that drives it. Returns the run id
/// immediately; the UI polls for progress.
pub fn start_run(registry: RunRegistry, db_url: String, agent: String, input: String) -> Uuid {
    let run_id = Uuid::new_v4();
    let state = RunState {
        run_id,
        agent: agent.clone(),
        input: input.clone(),
        phase: RunPhase::Starting,
        backend_pid: None,
        started_at_ms: now_ms(),
        finished_at_ms: None,
        envelope: None,
        messages: Vec::new(),
        traces: Vec::new(),
        error: None,
        cancelled: false,
    };
    if let Ok(mut map) = registry.lock() {
        map.insert(run_id, state);
    }
    tokio::spawn(run_task(registry, db_url, run_id, agent, input));
    run_id
}

async fn run_task(
    registry: RunRegistry,
    db_url: String,
    run_id: Uuid,
    agent: String,
    input: String,
) {
    let client = match db::connect(&db_url).await {
        Ok(c) => c,
        Err(e) => {
            update(&registry, run_id, |s| {
                s.phase = RunPhase::Failed;
                s.error = Some(e.to_string());
                s.finished_at_ms = Some(now_ms());
            });
            return;
        }
    };

    // Record the backend PID so /api/run/:id/cancel can pg_cancel_backend it.
    match client.query_one("SELECT pg_backend_pid()", &[]).await {
        Ok(row) => {
            let pid: i32 = row.get(0);
            update(&registry, run_id, |s| {
                s.backend_pid = Some(pid);
                s.phase = RunPhase::Running;
            });
        }
        Err(e) => {
            update(&registry, run_id, |s| {
                s.phase = RunPhase::Failed;
                s.error = Some(e.to_string());
                s.finished_at_ms = Some(now_ms());
            });
            return;
        }
    }

    let result = client
        .query_one("SELECT synapse.execute($1, $2)::text", &[&agent, &input])
        .await;

    match result {
        Ok(row) => {
            let text: String = row.get(0);
            let envelope: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);

            // Fetch persisted trace rows when the kernel minted an execution id.
            let exec_id = envelope
                .get("execution_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let mut messages = Vec::new();
            let mut traces = Vec::new();
            if let Some(id) = exec_id {
                messages = db::jsonb_rows(
                    &client,
                    "SELECT to_jsonb(m)::text FROM ( \
                       SELECT seq, role, content, tool_call_id, tool_name, tool_input, tool_output \
                       FROM synapse.messages WHERE execution_id = ($1::text)::uuid ORDER BY seq) m",
                    &[&id],
                )
                .await
                .unwrap_or_default();
                traces = db::jsonb_rows(
                    &client,
                    "SELECT to_jsonb(t)::text FROM ( \
                       SELECT seq, event, payload \
                       FROM synapse.traces WHERE execution_id = ($1::text)::uuid ORDER BY seq) t",
                    &[&id],
                )
                .await
                .unwrap_or_default();
            }

            update(&registry, run_id, |s| {
                s.phase = RunPhase::Done;
                s.envelope = Some(envelope);
                s.messages = messages;
                s.traces = traces;
                s.finished_at_ms = Some(now_ms());
            });
        }
        Err(e) => {
            // A pg_cancel_backend lands here when Postgres services the
            // interrupt after the kernel loop aborted (SQLSTATE 57014).
            let cancelled = e.code().map(|c| c.code() == "57014").unwrap_or(false);
            update(&registry, run_id, |s| {
                s.phase = RunPhase::Failed;
                s.cancelled = cancelled;
                s.error = Some(if cancelled {
                    "execution cancelled: statement cancelled by operator".to_owned()
                } else {
                    e.to_string()
                });
                s.finished_at_ms = Some(now_ms());
            });
        }
    }
}
