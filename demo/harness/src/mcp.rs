//! An MCP server, so other agents can use a pg-one app as a tool.
//!
//! pg_synapse already speaks MCP as a *client*: an agent here can call tools on
//! someone else's server. This is the other direction, and it is what makes a
//! built app reachable from Claude, from an IDE, or from any other agent
//! runtime without writing an integration.
//!
//! Hand-rolled JSON-RPC 2.0 rather than pulled from an SDK, to match
//! `core/src/tool/mcp_client.rs`, which hand-rolls the client half over the
//! same three methods and the same protocol version. One convention in the
//! codebase beats two.
//!
//! ponytail: implements initialize, tools/list and tools/call, which is what a
//! tool-using client needs. Resources, prompts, sampling and subscriptions are
//! not implemented; add them when something actually asks.
//!
//! Three generic tools rather than one per saved question. A tool list that
//! changes shape as data changes is harder for a client to cache and reason
//! about, and "ask this app this question" is a clearer contract than fifty
//! near-identical entries.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::db;
use crate::error::HarnessError;
use crate::AppState;

/// Matches the version the client half speaks.
const PROTOCOL_VERSION: &str = "2025-11-25";

fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_apps",
            "description": "List the applications available in this Postgres database, with the agents and saved questions each one has.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "ask",
            "description": "Answer a saved question about an app. Runs pre-approved SQL, so it is fast, free, and returns the same answer every time. Use list_apps to see what questions exist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app": { "type": "string", "description": "App name" },
                    "question": { "type": "string", "description": "Saved question name" }
                },
                "required": ["app", "question"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_agent",
            "description": "Run an agent with a natural-language instruction. Slower and non-deterministic than `ask`, because it calls a model; prefer `ask` when a saved question already answers the need.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Agent name" },
                    "input": { "type": "string", "description": "What the agent should do" }
                },
                "required": ["agent", "input"],
                "additionalProperties": false
            }
        }
    ])
}

/// One JSON-RPC endpoint handling the three methods a tool-using client needs.
pub async fn rpc(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, HarnessError> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "pg-one", "version": env!("CARGO_PKG_VERSION") }
        })),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(&state, &params).await,
        // A notification has no id and expects no reply; anything else is a
        // method we do not implement, and saying so is better than silence.
        other => Err(format!("method '{other}' is not implemented")),
    };

    Ok(Json(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        // MCP convention: a tool that fails reports it inside the result with
        // isError, so the model can read and react to it. A protocol-level
        // error is a different thing and stays in the error field.
        Err(message) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32603, "message": message }
        }),
    }))
}

async fn call_tool(state: &AppState, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let text = match name {
        "list_apps" => list_apps(state).await,
        "ask" => ask(state, &args).await,
        "run_agent" => run_agent(state, &args).await,
        other => Err(format!("no tool named '{other}'")),
    };
    Ok(match text {
        Ok(t) => json!({ "content": [{ "type": "text", "text": t }], "isError": false }),
        Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
    })
}

fn db_err(e: HarnessError) -> String {
    format!("{e}")
}

async fn list_apps(state: &AppState) -> Result<String, String> {
    let client = db::connect(&state.db_url).await.map_err(db_err)?;
    let apps = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(a)::text FROM ( \
           SELECT p.name, p.schema_name, \
             COALESCE((SELECT array_agg(ag.agent ORDER BY ag.agent) \
                       FROM synapse.app_agents ag WHERE ag.app = p.name), '{}') AS agents, \
             COALESCE((SELECT json_agg(json_build_object('name', q.name, 'asks', q.nl_text) \
                       ORDER BY q.name) \
                       FROM synapse.questions q \
                       WHERE q.app = p.name AND q.confirmed_at IS NOT NULL), '[]'::json) AS questions \
           FROM synapse.apps p ORDER BY p.name) a",
        &[],
    )
    .await
    .map_err(db_err)?;
    serde_json::to_string_pretty(&apps).map_err(|e| e.to_string())
}

async fn ask(state: &AppState, args: &Value) -> Result<String, String> {
    let app = args.get("app").and_then(Value::as_str).unwrap_or("");
    let question = args.get("question").and_then(Value::as_str).unwrap_or("");
    let client = db::connect(&state.db_url).await.map_err(db_err)?;
    let rows = client
        .query(
            "SELECT sql_text, (confirmed_at IS NOT NULL) FROM synapse.questions \
             WHERE app = $1 AND name = $2 AND kind = 'sql'",
            &[&app, &question],
        )
        .await
        .map_err(|e| e.to_string())?;
    let row = rows
        .first()
        .ok_or_else(|| format!("no saved question '{question}' for app '{app}'"))?;
    let confirmed: bool = row.get(1);
    if !confirmed {
        // The review gate applies to every surface, not just the UI. A question
        // nobody approved is not answerable over MCP either.
        return Err(format!("question '{question}' has not been reviewed"));
    }
    let sql: String = row.get::<_, Option<String>>(0).unwrap_or_default();
    let out = db::jsonb_rows(
        &client,
        &format!("SELECT to_jsonb(r)::text FROM ({sql}) r"),
        &[],
    )
    .await
    .map_err(db_err)?;
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

async fn run_agent(state: &AppState, args: &Value) -> Result<String, String> {
    let agent = args.get("agent").and_then(Value::as_str).unwrap_or("");
    let input = args.get("input").and_then(Value::as_str).unwrap_or("");
    let client = db::connect(&state.db_url).await.map_err(db_err)?;
    let row = client
        .query_one("SELECT synapse.execute($1, $2)::text", &[&agent, &input])
        .await
        .map_err(|e| e.to_string())?;
    let env: Value = serde_json::from_str(&row.get::<_, String>(0)).unwrap_or(Value::Null);
    if let Some(err) = env.get("error").and_then(Value::as_str) {
        return Err(err.to_owned());
    }
    Ok(env
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned())
}
