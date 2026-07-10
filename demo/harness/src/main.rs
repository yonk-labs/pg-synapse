//! pg_synapse demo harness: a thin axum server between the static web UI and
//! Postgres. It wraps the `synapse.*` SQL surface as JSON endpoints; all
//! agent state lives in Postgres, the only in-memory state is the run
//! registry used for live polling and cancellation.

mod api;
mod db;
mod error;
mod runs;
mod scenarios;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;

#[derive(Clone)]
pub struct AppState {
    pub db_url: String,
    pub runs: runs::RunRegistry,
    pub default_llm_base_url: String,
    pub default_llm_model: String,
}

const INDEX_HTML: &str = include_str!("../static/index.html");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[tokio::main]
async fn main() {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "host=localhost port=5432 user=postgres password=postgres dbname=synapse_demo".to_owned()
    });
    let addr = std::env::var("HARNESS_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    // The repo's documented test endpoint; the UI form defaults to it and the
    // presenter overrides it at runtime.
    let default_llm_base_url = std::env::var("DEFAULT_LLM_BASE_URL")
        .unwrap_or_else(|_| "http://192.168.1.193:8000/v1".to_owned());
    let default_llm_model = std::env::var("DEFAULT_LLM_MODEL")
        .unwrap_or_else(|_| "Intel/Qwen3-Coder-Next-int4-AutoRound".to_owned());

    let state = AppState {
        db_url,
        runs: Arc::new(Mutex::new(HashMap::new())),
        default_llm_base_url,
        default_llm_model,
    };

    let app = Router::new()
        .route("/", get(index))
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
        .route("/api/schema/tables", get(api::schema_tables))
        .route("/api/schema/columns", get(api::schema_columns))
        .route("/api/schema/rows", get(api::schema_rows))
        .route("/api/schema/update", post(api::schema_update))
        .route("/api/schema/insert", post(api::schema_insert))
        .route("/api/sql", post(api::run_sql))
        .route("/api/probe/{key}", get(api::probe))
        .route("/api/execution/{execution_id}", get(api::execution_detail))
        .route("/api/scenario/{id}", post(api::scenario_load))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));
    println!("pg_synapse demo harness listening on http://{addr}");
    axum::serve(listener, app)
        .await
        .expect("axum server crashed");
}
