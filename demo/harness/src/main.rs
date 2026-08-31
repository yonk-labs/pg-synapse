//! pg_synapse demo harness: a thin axum server between the static web UI and
//! Postgres. It wraps the `synapse.*` SQL surface as JSON endpoints; all
//! agent state lives in Postgres, the only in-memory state is the run
//! registry used for live polling and cancellation.

mod api;
mod db;
mod error;
mod questions;
mod runs;
mod scenarios;
mod schedules;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;

/// Running continuous-workload generators, keyed by target. The bool is the
/// stop flag the background task polls.
pub type WorkloadRegistry = Arc<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>;

#[derive(Clone)]
pub struct AppState {
    pub db_url: String,
    pub runs: runs::RunRegistry,
    pub workloads: WorkloadRegistry,
    pub default_llm_base_url: String,
    pub default_llm_model: String,
    /// Directory shared with the `db` container at the same absolute path
    /// (the tools-fs sandbox root there is `/tmp/pg_synapse_fs`; see
    /// docker-compose.yml). A path this harness writes is directly usable
    /// by an agent's `read_file` tool with no translation.
    pub upload_dir: String,
}

const INDEX_HTML: &str = include_str!("../static/index.html");
const SIDECAR_HTML: &str = include_str!("../static/sidecar.html");
const PGONE_HTML: &str = include_str!("../static/pgone.html");
/// Pre-written, not agent-generated: realistic messy product-review data for
/// pg-one's "use sample data" button, so the file-upload -> normalize flow
/// has something real to work with without the builder agent having to
/// invent source data during its own run.
pub const SAMPLE_REVIEWS_CSV: &str = include_str!("../sample-data/product_reviews_sample.csv");
pub const SAMPLE_TICKETS_CSV: &str = include_str!("../sample-data/support_tickets_sample.csv");
pub const SAMPLE_EXPENSES_CSV: &str = include_str!("../sample-data/expenses_sample.csv");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn sidecar_page() -> Html<&'static str> {
    Html(SIDECAR_HTML)
}

async fn pgone_page() -> Html<&'static str> {
    Html(PGONE_HTML)
}

#[tokio::main]
async fn main() {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "host=localhost port=5432 user=postgres password=postgres dbname=synapse_demo".to_owned()
    });
    let addr = std::env::var("HARNESS_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    // A generic placeholder, not a real endpoint: no personal/internal
    // address belongs baked into this binary. The UI form prefills with
    // this and the presenter points it at their own server at runtime;
    // override via DEFAULT_LLM_BASE_URL/DEFAULT_LLM_MODEL (docker-compose.yml
    // reads these from a local, gitignored .env, not a tracked default).
    let default_llm_base_url = std::env::var("DEFAULT_LLM_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_owned());
    let default_llm_model =
        std::env::var("DEFAULT_LLM_MODEL").unwrap_or_else(|_| "local-model".to_owned());
    let upload_dir =
        std::env::var("UPLOAD_DIR").unwrap_or_else(|_| "/tmp/pg_synapse_fs/uploads".to_owned());
    std::fs::create_dir_all(&upload_dir)
        .unwrap_or_else(|e| panic!("cannot create upload dir {upload_dir}: {e}"));

    let state = AppState {
        db_url,
        runs: Arc::new(Mutex::new(HashMap::new())),
        workloads: Arc::new(Mutex::new(HashMap::new())),
        default_llm_base_url,
        default_llm_model,
        upload_dir,
    };

    // The scheduler driver. pg_cron is the better answer for a deployment the
    // user owns (it survives this process dying), but it needs
    // shared_preload_libraries and a restart, which a demo container should
    // not demand. See schedules::spawn_driver.
    let scheduler_secs: u64 = std::env::var("SCHEDULER_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    schedules::spawn_driver(state.db_url.clone(), scheduler_secs);

    let app = Router::new()
        .route("/", get(index))
        .route("/sidecar", get(sidecar_page))
        .route("/pgone", get(pgone_page))
        .route("/api/sidecar/probe", post(api::sidecar_probe))
        .route("/api/sidecar/execute", post(api::sidecar_execute))
        .route("/api/workload/seed", post(api::workload_seed))
        .route("/api/workload/reset", post(api::workload_reset))
        .route("/api/workload/start", post(api::workload_start))
        .route("/api/workload/stop", post(api::workload_stop))
        .route("/api/workload/status", get(api::workload_status))
        .route("/api/bootstrap", get(api::bootstrap))
        .route("/api/profile", post(api::profile_set))
        .route("/api/profile/test", post(api::profile_test))
        .route("/api/agent", post(api::agent_set))
        .route("/api/agent/drop", post(api::agent_drop))
        .route("/api/execute", post(api::execute))
        .route("/api/run/{run_id}", get(api::run_status))
        .route("/api/run/{run_id}/cancel", post(api::run_cancel))
        .route("/api/trigger/attach", post(api::trigger_attach))
        .route("/api/trigger/detach", post(api::trigger_detach))
        .route("/api/demo/ticket", post(api::insert_ticket))
        .route("/api/demo/order", post(api::insert_order))
        .route("/api/drain", post(api::drain_queue))
        .route("/api/table/{name}", get(api::table_view))
        .route("/api/schema/tree", get(api::schema_tree))
        .route("/api/schema/tables", get(api::schema_tables))
        .route("/api/schema/columns", get(api::schema_columns))
        .route("/api/schema/rows", get(api::schema_rows))
        .route("/api/schema/update", post(api::schema_update))
        .route("/api/schema/insert", post(api::schema_insert))
        .route("/api/sql", post(api::run_sql))
        .route("/api/probe/{key}", get(api::probe))
        .route("/api/execution/{execution_id}", get(api::execution_detail))
        .route("/api/scenario/{id}", post(api::scenario_load))
        .route("/api/upload", post(api::upload_file))
        .route("/api/upload/sample", post(api::upload_sample))
        .route(
            "/api/connection",
            get(api::connection_list).post(api::connection_add),
        )
        .route("/api/apps", get(schedules::app_list))
        .route("/api/runs", get(schedules::all_runs))
        .route("/api/app/{app}/schedules", get(schedules::schedule_list))
        .route("/api/app/{app}/runs", get(schedules::app_runs))
        .route("/api/schedule", post(schedules::schedule_add))
        .route("/api/schedule/{id}/drop", post(schedules::schedule_drop))
        .route("/api/tick", post(schedules::tick))
        .route("/api/samples", get(api::sample_list))
        .route("/api/sample/{name}", get(api::sample_download))
        .route("/api/question", post(questions::save))
        .route("/api/app/{app}/questions", get(questions::list))
        .route("/api/app/{app}/q/{name}", get(questions::run))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));
    println!("pg_synapse demo harness listening on http://{addr}");
    axum::serve(listener, app)
        .await
        .expect("axum server crashed");
}
