//! axum router and handlers for the 12 v1 endpoints (decisions D7).
//!
//! All handlers are thin wrappers that delegate to the kernel Runtime or write
//! rows via the sqlx pool. Error responses use consistent JSON bodies:
//! `{"error": "<message>"}`.
//!
//! ## Admin auth
//!
//! Endpoints under `/v1/admin/*` require the `X-PG-Synapse-Admin-Token`
//! header to match `--admin-token`. If the sidecar was started without
//! `--admin-token`, admin endpoints return 503 with a clear message.
//!
//! ## Async execution
//!
//! `execute_async` inserts a queued row into `synapse.executions`, runs the
//! agent inline (same sync-under-the-hood approach as the pgrx N2 path), and
//! returns the execution_id immediately. The status can be polled via
//! `GET /v1/status/:execution_id`. A true background-worker queue is a v0.2
//! refinement.

#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{error, info, instrument};
use uuid::Uuid;

use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/execute` and `POST /v1/execute_async`.
#[derive(Debug, Deserialize)]
pub struct ExecuteRequest {
    /// Agent name (must exist in the runtime).
    pub agent: String,
    /// User input text.
    pub input: String,
    /// Optional Postgres role to propagate to tools.
    pub caller_role: Option<String>,
}

/// Request body for `POST /v1/embed`.
#[derive(Debug, Deserialize)]
pub struct EmbedRequest {
    /// Text to embed.
    pub text: String,
    /// Optional embedding profile name. Falls back to the default profile.
    pub profile: Option<String>,
}

/// Request body for `POST /v1/tool_call`.
#[derive(Debug, Deserialize)]
pub struct ToolCallRequest {
    /// Registered tool name.
    pub tool: String,
    /// Tool input as a JSON value.
    pub input: Value,
    /// Optional caller role propagated to the tool executor.
    pub caller_role: Option<String>,
}

/// Request body for `POST /v1/admin/agent`.
#[derive(Debug, Deserialize)]
pub struct AdminAgentRequest {
    pub name: String,
    pub system_prompt: String,
    pub soul: Option<String>,
    pub executor_name: Option<String>,
    pub llm_profile_main: Option<String>,
    pub llm_profile_small: Option<String>,
    pub llm_profile_judge: Option<String>,
    pub embedding_profile: Option<String>,
    pub tools: Option<Vec<String>>,
    pub max_iterations: Option<i32>,
    pub timeout_ms: Option<i64>,
    pub cost_cap_usd: Option<f64>,
}

/// Request body for `POST /v1/admin/profile/llm`.
#[derive(Debug, Deserialize)]
pub struct AdminLlmProfileRequest {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub api_key_secret: Option<String>,
    pub base_url: Option<String>,
    pub params: Option<Value>,
}

/// Request body for `POST /v1/admin/profile/embedding`.
#[derive(Debug, Deserialize)]
pub struct AdminEmbeddingProfileRequest {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub dimension: i32,
    pub api_key_secret: Option<String>,
    pub base_url: Option<String>,
    pub params: Option<Value>,
}

/// Request body for `POST /v1/admin/secret`.
#[derive(Debug, Deserialize)]
pub struct AdminSecretRequest {
    pub name: String,
    pub value: String,
}

/// Request body for `POST /v1/admin/tool`.
#[derive(Debug, Deserialize)]
pub struct AdminToolRequest {
    pub name: String,
    pub description: Option<String>,
    pub schema_json: Value,
    pub kind: Option<String>,
    pub config: Option<Value>,
}

// ---------------------------------------------------------------------------
// Router construction
// ---------------------------------------------------------------------------

/// Build the axum router with all 12 v1 endpoints wired to their handlers.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        // Core endpoints
        .route("/v1/execute", post(execute_handler))
        .route("/v1/execute_async", post(execute_async_handler))
        .route("/v1/status/{execution_id}", get(status_handler))
        .route("/v1/embed", post(embed_handler))
        .route("/v1/tool_call", post(tool_call_handler))
        // Info endpoints
        .route("/v1/health", get(health_handler))
        .route("/v1/version", get(version_handler))
        // Admin endpoints
        .route("/v1/admin/agent", post(admin_agent_handler))
        .route("/v1/admin/profile/llm", post(admin_llm_profile_handler))
        .route(
            "/v1/admin/profile/embedding",
            post(admin_embedding_profile_handler),
        )
        .route("/v1/admin/secret", post(admin_secret_handler))
        .route("/v1/admin/tool", post(admin_tool_handler))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Helper: JSON error response
// ---------------------------------------------------------------------------

fn err_json(status: StatusCode, msg: impl std::fmt::Display) -> impl IntoResponse {
    (status, Json(json!({ "error": msg.to_string() })))
}

// ---------------------------------------------------------------------------
// Helper: admin auth check
// ---------------------------------------------------------------------------

fn check_admin_token(headers: &HeaderMap, state: &AppState) -> Result<(), impl IntoResponse> {
    match &state.admin_token {
        None => Err(err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin endpoints are disabled: start sidecar with --admin-token",
        )),
        Some(token) => {
            let provided = headers
                .get("x-pg-synapse-admin-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if provided == token {
                Ok(())
            } else {
                Err(err_json(
                    StatusCode::UNAUTHORIZED,
                    "invalid or missing X-PG-Synapse-Admin-Token",
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /v1/health
async fn health_handler() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// GET /v1/version
async fn version_handler() -> Json<Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
}

/// POST /v1/execute
#[instrument(skip(state, body), fields(agent = %body.agent))]
async fn execute_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExecuteRequest>,
) -> impl IntoResponse {
    info!("execute agent={}", body.agent);
    let result = state
        .runtime
        .execute_with_caller(&body.agent, &body.input, body.caller_role)
        .await;

    match result {
        Ok(outcome) => (
            StatusCode::OK,
            Json(json!({
                "output": outcome.output,
                "tokens_in": outcome.tokens_in,
                "tokens_out": outcome.tokens_out,
                "cost_usd": outcome.cost_usd,
                "duration_ms": outcome.duration_ms,
            })),
        )
            .into_response(),
        Err(e) => {
            error!("execute error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/execute_async
///
/// Inserts a queued execution row, runs the agent inline (same sync-under-
/// the-hood approach as the pgrx async path; a real background worker queue
/// is deferred to v0.2), and returns the execution_id immediately.
#[instrument(skip(state, body), fields(agent = %body.agent))]
async fn execute_async_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExecuteRequest>,
) -> impl IntoResponse {
    let execution_id = Uuid::new_v4();
    info!("execute_async agent={} id={}", body.agent, execution_id);

    // Insert a queued row first so callers can poll status immediately.
    let insert = sqlx::query(
        "INSERT INTO synapse.executions \
         (execution_id, agent_name, input, status, caller_role) \
         VALUES ($1, $2, $3, 'queued', $4)",
    )
    .bind(execution_id)
    .bind(&body.agent)
    .bind(&body.input)
    .bind(body.caller_role.as_deref())
    .execute(&state.pool)
    .await;

    if let Err(e) = insert {
        error!("execute_async insert error: {e}");
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    // Spawn inline execution. Real queue (background worker) is v0.2.
    let state2 = state.clone();
    let agent = body.agent.clone();
    let input = body.input.clone();
    let caller_role = body.caller_role.clone();
    tokio::spawn(async move {
        let outcome = state2
            .runtime
            .execute_with_caller(&agent, &input, caller_role)
            .await;

        let (status, output, tokens_in, tokens_out, cost) = match outcome {
            Ok(o) => (
                "completed",
                Some(o.output),
                o.tokens_in as i64,
                o.tokens_out as i64,
                o.cost_usd,
            ),
            Err(e) => {
                error!("execute_async agent error: {e}");
                ("failed", Some(e.to_string()), 0i64, 0i64, None)
            }
        };

        let _ = sqlx::query(
            "UPDATE synapse.executions \
             SET status=$1, output=$2, tokens_in=$3, tokens_out=$4, \
                 cost_usd=$5::numeric, finished_at=now(), \
                 duration_ms=EXTRACT(EPOCH FROM (now()-started_at))*1000 \
             WHERE execution_id=$6",
        )
        .bind(status)
        .bind(output)
        .bind(tokens_in)
        .bind(tokens_out)
        .bind(cost.map(|c: f64| c.to_string()))
        .bind(execution_id)
        .execute(&state2.pool)
        .await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "execution_id": execution_id })),
    )
        .into_response()
}

/// GET /v1/status/:execution_id
#[instrument(skip(state))]
async fn status_handler(
    State(state): State<Arc<AppState>>,
    Path(execution_id): Path<Uuid>,
) -> impl IntoResponse {
    let row = sqlx::query(
        "SELECT execution_id, agent_name, status, output, \
                tokens_in, tokens_out, cost_usd::text as cost_usd_text, \
                started_at, finished_at \
         FROM synapse.executions \
         WHERE execution_id = $1",
    )
    .bind(execution_id)
    .fetch_optional(&state.pool)
    .await;

    match row {
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            format!("execution {execution_id} not found"),
        )
        .into_response(),
        Ok(Some(r)) => {
            use sqlx::Row;
            let status: String = r.try_get("status").unwrap_or_default();
            let output: Option<String> = r.try_get("output").unwrap_or(None);
            let tokens_in: i32 = r.try_get("tokens_in").unwrap_or(0);
            let tokens_out: i32 = r.try_get("tokens_out").unwrap_or(0);
            let cost: Option<f64> = r
                .try_get::<Option<String>, _>("cost_usd_text")
                .unwrap_or(None)
                .and_then(|s| s.parse::<f64>().ok());
            (
                StatusCode::OK,
                Json(json!({
                    "execution_id": execution_id,
                    "status": status,
                    "output": output,
                    "tokens_in": tokens_in,
                    "tokens_out": tokens_out,
                    "cost_usd": cost,
                })),
            )
                .into_response()
        }
    }
}

/// POST /v1/embed
#[instrument(skip(state, body))]
async fn embed_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmbedRequest>,
) -> impl IntoResponse {
    match state
        .runtime
        .embed(&body.text, body.profile.as_deref())
        .await
    {
        Ok(vec) => {
            let dim = vec.dimension();
            let floats: Vec<f32> = vec.into_inner();
            (
                StatusCode::OK,
                Json(json!({ "vector": floats, "dimension": dim })),
            )
                .into_response()
        }
        Err(e) => {
            error!("embed error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/tool_call
#[instrument(skip(state, body), fields(tool = %body.tool))]
async fn tool_call_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ToolCallRequest>,
) -> impl IntoResponse {
    match state
        .runtime
        .call_tool(&body.tool, body.input, body.caller_role)
        .await
    {
        Ok(out) => (StatusCode::OK, Json(json!({ "output": out }))).into_response(),
        Err(e) => {
            error!("tool_call error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/admin/agent
#[instrument(skip(state, headers, body), fields(name = %body.name))]
async fn admin_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AdminAgentRequest>,
) -> impl IntoResponse {
    if let Err(r) = check_admin_token(&headers, &state) {
        return r.into_response();
    }

    let tools: Vec<String> = body.tools.unwrap_or_default();
    let result = sqlx::query(
        "INSERT INTO synapse.agents \
         (name, system_prompt, soul, executor_name, llm_profile_main, \
          llm_profile_small, llm_profile_judge, embedding_profile, \
          tools, max_iterations, timeout_ms, cost_cap_usd) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12::numeric) \
         ON CONFLICT (name) DO UPDATE SET \
           system_prompt=EXCLUDED.system_prompt, soul=EXCLUDED.soul, \
           executor_name=EXCLUDED.executor_name, \
           llm_profile_main=EXCLUDED.llm_profile_main, \
           llm_profile_small=EXCLUDED.llm_profile_small, \
           llm_profile_judge=EXCLUDED.llm_profile_judge, \
           embedding_profile=EXCLUDED.embedding_profile, \
           tools=EXCLUDED.tools, max_iterations=EXCLUDED.max_iterations, \
           timeout_ms=EXCLUDED.timeout_ms, cost_cap_usd=EXCLUDED.cost_cap_usd, \
           updated_at=now()",
    )
    .bind(&body.name)
    .bind(&body.system_prompt)
    .bind(&body.soul)
    .bind(body.executor_name.as_deref().unwrap_or("conversation"))
    .bind(&body.llm_profile_main)
    .bind(&body.llm_profile_small)
    .bind(&body.llm_profile_judge)
    .bind(&body.embedding_profile)
    .bind(&tools)
    .bind(body.max_iterations.unwrap_or(10))
    .bind(body.timeout_ms.unwrap_or(60_000))
    .bind(body.cost_cap_usd.map(|c| c.to_string()))
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "name": body.name })),
        )
            .into_response(),
        Err(e) => {
            error!("admin_agent error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/admin/profile/llm
#[instrument(skip(state, headers, body), fields(name = %body.name))]
async fn admin_llm_profile_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AdminLlmProfileRequest>,
) -> impl IntoResponse {
    if let Err(r) = check_admin_token(&headers, &state) {
        return r.into_response();
    }

    let params = body.params.unwrap_or(json!({}));
    let result = sqlx::query(
        "INSERT INTO synapse.llm_profiles \
         (name, provider, model, api_key_secret, base_url, params) \
         VALUES ($1,$2,$3,$4,$5,$6) \
         ON CONFLICT (name) DO UPDATE SET \
           provider=EXCLUDED.provider, model=EXCLUDED.model, \
           api_key_secret=EXCLUDED.api_key_secret, base_url=EXCLUDED.base_url, \
           params=EXCLUDED.params, updated_at=now()",
    )
    .bind(&body.name)
    .bind(&body.provider)
    .bind(&body.model)
    .bind(&body.api_key_secret)
    .bind(&body.base_url)
    .bind(sqlx::types::Json(&params))
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "name": body.name })),
        )
            .into_response(),
        Err(e) => {
            error!("admin_llm_profile error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/admin/profile/embedding
#[instrument(skip(state, headers, body), fields(name = %body.name))]
async fn admin_embedding_profile_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AdminEmbeddingProfileRequest>,
) -> impl IntoResponse {
    if let Err(r) = check_admin_token(&headers, &state) {
        return r.into_response();
    }

    let params = body.params.unwrap_or(json!({}));
    let result = sqlx::query(
        "INSERT INTO synapse.embedding_profiles \
         (name, provider, model, dimension, api_key_secret, base_url, params) \
         VALUES ($1,$2,$3,$4,$5,$6,$7) \
         ON CONFLICT (name) DO UPDATE SET \
           provider=EXCLUDED.provider, model=EXCLUDED.model, \
           dimension=EXCLUDED.dimension, \
           api_key_secret=EXCLUDED.api_key_secret, base_url=EXCLUDED.base_url, \
           params=EXCLUDED.params, updated_at=now()",
    )
    .bind(&body.name)
    .bind(&body.provider)
    .bind(&body.model)
    .bind(body.dimension)
    .bind(&body.api_key_secret)
    .bind(&body.base_url)
    .bind(sqlx::types::Json(&params))
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "name": body.name })),
        )
            .into_response(),
        Err(e) => {
            error!("admin_embedding_profile error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/admin/secret
#[instrument(skip(state, headers, body), fields(name = %body.name))]
async fn admin_secret_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AdminSecretRequest>,
) -> impl IntoResponse {
    if let Err(r) = check_admin_token(&headers, &state) {
        return r.into_response();
    }

    let result = sqlx::query(
        "INSERT INTO synapse.secrets (name, value) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET value=EXCLUDED.value, updated_at=now()",
    )
    .bind(&body.name)
    .bind(&body.value)
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "name": body.name })),
        )
            .into_response(),
        Err(e) => {
            error!("admin_secret error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

/// POST /v1/admin/tool
#[instrument(skip(state, headers, body), fields(name = %body.name))]
async fn admin_tool_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AdminToolRequest>,
) -> impl IntoResponse {
    if let Err(r) = check_admin_token(&headers, &state) {
        return r.into_response();
    }

    let config = body.config.unwrap_or(json!({}));
    let result = sqlx::query(
        "INSERT INTO synapse.tools (name, description, schema_json, kind, config) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (name) DO UPDATE SET \
           description=EXCLUDED.description, schema_json=EXCLUDED.schema_json, \
           kind=EXCLUDED.kind, config=EXCLUDED.config",
    )
    .bind(&body.name)
    .bind(&body.description)
    .bind(sqlx::types::Json(&body.schema_json))
    .bind(body.kind.as_deref().unwrap_or("external"))
    .bind(sqlx::types::Json(&config))
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "name": body.name })),
        )
            .into_response(),
        Err(e) => {
            error!("admin_tool error: {e}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests (no Docker; axum oneshot)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use pg_synapse_core::Runtime;
    use sqlx::PgPool;
    use tower::ServiceExt;

    /// Build a minimal AppState without a real DB.
    async fn test_state(admin_token: Option<&str>) -> Arc<AppState> {
        // Build a bare Runtime (no providers loaded); we only test endpoints
        // that do NOT call the runtime or pool.
        let runtime = Runtime::builder().build().await.unwrap();

        // connect_lazy does not open a connection, so no DB is required.
        let pool = PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap();

        Arc::new(AppState {
            runtime,
            pool,
            admin_token: admin_token.map(str::to_string),
        })
    }

    async fn app(admin_token: Option<&str>) -> Router {
        router(test_state(admin_token).await)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = app(None)
            .await
            .oneshot(
                Request::builder()
                    .uri("/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "ok");
    }

    #[tokio::test]
    async fn version_returns_package_version() {
        let response = app(None)
            .await
            .oneshot(
                Request::builder()
                    .uri("/v1/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(!v["version"].as_str().unwrap_or("").is_empty());
    }

    #[tokio::test]
    async fn admin_without_token_configured_returns_503() {
        let response = app(None)
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/admin/secret")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"k","value":"v"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn admin_with_wrong_token_returns_401() {
        let response = app(Some("correct-token"))
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/admin/secret")
                    .header("content-type", "application/json")
                    .header("x-pg-synapse-admin-token", "wrong-token")
                    .body(Body::from(r#"{"name":"k","value":"v"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn execute_unknown_agent_returns_500() {
        let response = app(None)
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/execute")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"agent":"ghost","input":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn embed_no_profile_returns_500() {
        let response = app(None)
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embed")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn tool_call_unknown_tool_returns_500() {
        let response = app(None)
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/tool_call")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tool":"nope","input":{}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
