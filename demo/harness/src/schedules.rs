//! Schedules: the difference between an automation and a system.
//!
//! An agent that runs once produces a result; an agent that runs on a schedule
//! produces a dataset, and only a dataset can be mined. The mechanism already
//! existed (`synapse.agent_queue`, `enqueue`, `drain_queue`); what was missing
//! was a row saying "run this every day at nine".
//!
//! This module is a transport. Scheduling decisions live in `synapse.tick()`,
//! which any driver (pg_cron, a poller, a systemd timer) can call.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db;
use crate::error::HarnessError;
use crate::AppState;

pub async fn app_list(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let apps = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(a)::text FROM ( \
           SELECT p.name, p.title, p.schema_name, p.connection, p.created_at, \
                  COALESCE(array_agg(ag.agent ORDER BY ag.agent) \
                           FILTER (WHERE ag.agent IS NOT NULL), '{}') AS agents \
           FROM synapse.apps p \
           LEFT JOIN synapse.app_agents ag ON ag.app = p.name \
           GROUP BY p.name, p.title, p.schema_name, p.connection, p.created_at \
           ORDER BY p.name) a",
        &[],
    )
    .await?;
    Ok(Json(json!({"ok": true, "apps": apps})))
}

#[derive(Deserialize)]
pub struct ScheduleReq {
    pub app: String,
    pub agent: String,
    pub input: String,
    /// A Postgres interval literal, for example "1 day" or "6 hours".
    pub every: String,
    /// When the first run should happen. Alignment comes from this: a first run
    /// at 09:00 with an interval of one day stays at 09:00 forever.
    #[serde(default)]
    pub first_run_at: Option<String>,
}

pub async fn schedule_add(
    State(state): State<AppState>,
    Json(req): Json<ScheduleReq>,
) -> Result<Json<Value>, HarnessError> {
    if req.input.trim().is_empty() {
        return Err(HarnessError::BadRequest(
            "a schedule needs something for the agent to do".to_owned(),
        ));
    }
    let client = db::connect(&state.db_url).await?;
    // `every` and `first_run_at` are cast inside Postgres rather than parsed
    // here, so an unparseable interval fails as a clean database error instead
    // of a bespoke and probably wrong parser.
    let row = client
        .query_one(
            "INSERT INTO synapse.schedules (app, agent, input, every_interval, next_run_at) \
             VALUES ($1, $2, $3, $4::interval, \
                     COALESCE($5::timestamptz, now() + $4::interval)) \
             RETURNING schedule_id::text",
            &[
                &req.app,
                &req.agent,
                &req.input,
                &req.every,
                &req.first_run_at,
            ],
        )
        .await
        .map_err(|e| HarnessError::BadRequest(format!("could not create schedule: {e}")))?;
    let id: String = row.get(0);
    Ok(Json(json!({"ok": true, "schedule_id": id})))
}

pub async fn schedule_list(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let schedules = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(s)::text FROM ( \
           SELECT schedule_id::text AS schedule_id, agent, input, \
                  every_interval::text AS every, next_run_at, last_run_at, enabled \
           FROM synapse.schedules WHERE app = $1 ORDER BY next_run_at) s",
        &[&app],
    )
    .await?;
    Ok(Json(
        json!({"ok": true, "app": app, "schedules": schedules}),
    ))
}

pub async fn schedule_drop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let n = client
        .execute(
            "DELETE FROM synapse.schedules WHERE schedule_id = $1::uuid",
            &[&id],
        )
        .await
        .map_err(|e| HarnessError::BadRequest(format!("could not delete schedule: {e}")))?;
    if n == 0 {
        return Err(HarnessError::NotFound(format!("no schedule {id}")));
    }
    Ok(Json(json!({"ok": true})))
}

/// Run every due schedule now. The same entry point a scheduler driver calls,
/// exposed so the UI can offer a "run it now" without waiting for the clock.
pub async fn tick(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let fired: i32 = client.query_one("SELECT synapse.tick()", &[]).await?.get(0);
    Ok(Json(json!({"ok": true, "fired": fired})))
}

/// Per app run history: what the schedule actually did, including the runs that
/// failed. A schedule you cannot see failing is worse than no schedule.
pub async fn app_runs(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let runs = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(r)::text FROM ( \
           SELECT e.agent_name, e.status, e.model, e.caller_role, \
                  e.tokens_in, e.tokens_out, e.duration_ms, e.started_at, \
                  left(coalesce(e.output, ''), 200) AS output \
           FROM synapse.executions e \
           JOIN synapse.app_agents ag ON ag.agent = e.agent_name \
           WHERE ag.app = $1 \
           ORDER BY e.started_at DESC LIMIT 20) r",
        &[&app],
    )
    .await?;
    Ok(Json(json!({"ok": true, "app": app, "runs": runs})))
}

/// Every run across every app, newest first: the activity log.
///
/// Failures are included rather than filtered. A run history that shows only
/// successes is the same lie the queue used to tell by marking timed-out jobs
/// done, and it is the first place someone looks when an app stops producing.
pub async fn all_runs(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let runs = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(r)::text FROM ( \
           SELECT e.execution_id::text AS execution_id, e.agent_name, e.status, e.model, \
                  e.caller_role, e.tokens_in, e.tokens_out, e.cost_usd::float8 AS cost_usd, \
                  e.duration_ms, e.started_at, \
                  left(coalesce(e.input, ''), 160) AS input, \
                  left(coalesce(e.output, ''), 400) AS output, \
                  (ag.app IS NOT NULL) AS is_app \
           FROM synapse.executions e \
           LEFT JOIN synapse.app_agents ag ON ag.agent = e.agent_name \
           ORDER BY e.started_at DESC LIMIT 100) r",
        &[],
    )
    .await?;
    let stats = db::jsonb_one(
        &client,
        "SELECT to_jsonb(s)::text FROM ( \
           SELECT count(*) AS total, \
                  count(*) FILTER (WHERE status = 'completed') AS completed, \
                  count(*) FILTER (WHERE status <> 'completed') AS failed, \
                  count(*) FILTER (WHERE started_at > now() - interval '24 hours') AS last_24h, \
                  coalesce(round(avg(duration_ms))::bigint, 0) AS avg_ms, \
                  coalesce(sum(tokens_in + tokens_out), 0) AS tokens \
           FROM synapse.executions) s",
        &[],
    )
    .await?;
    let queue = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(q)::text FROM ( \
           SELECT agent, status, source, enqueued_at, \
                  left(coalesce(error, ''), 160) AS error \
           FROM synapse.agent_queue ORDER BY enqueued_at DESC LIMIT 20) q",
        &[],
    )
    .await?;
    Ok(Json(
        json!({"ok": true, "runs": runs, "stats": stats, "queue": queue}),
    ))
}

/// The scheduler driver: call `synapse.tick()` on a cadence, then drain what it
/// enqueued.
///
/// `synapse.tick()` deliberately has no opinion about who calls it, so any
/// driver works: pg_cron inside the database, a systemd timer, or this. This
/// one exists because pg_cron needs `shared_preload_libraries` and a restart,
/// which a container running someone's demo should not require. **For a
/// deployment the user owns, pg_cron is the better driver**: it survives the
/// harness being down, and scheduling should not depend on a web process.
///
/// Set `SCHEDULER_INTERVAL_SECS=0` to turn it off entirely (NN-7: every
/// capability has an off switch).
pub fn spawn_driver(db_url: String, every_secs: u64) {
    if every_secs == 0 {
        println!("scheduler driver disabled (SCHEDULER_INTERVAL_SECS=0)");
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(every_secs));
        // A slow drain must not cause a burst of catch-up ticks afterwards.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            // Errors are logged and swallowed on purpose: a database blip must
            // not kill the driver, or scheduling silently stops for good.
            match db::connect(&db_url).await {
                Ok(client) => {
                    let fired: i64 = match client.query_one("SELECT synapse.tick()", &[]).await {
                        Ok(row) => row.get::<_, i32>(0) as i64,
                        Err(e) => {
                            eprintln!("scheduler tick failed: {e}");
                            continue;
                        }
                    };
                    if fired > 0 {
                        println!("scheduler fired {fired} job(s)");
                        // Draining here rather than in a second driver keeps
                        // "due" and "run" adjacent: a job enqueued by a tick
                        // nobody drains is just a differently shaped silence.
                        if let Err(e) = client.query_one("SELECT synapse.drain_queue(5)", &[]).await
                        {
                            eprintln!("scheduler drain failed: {e}");
                        }
                    }
                }
                Err(e) => eprintln!("scheduler could not connect: {e}"),
            }
        }
    });
}
