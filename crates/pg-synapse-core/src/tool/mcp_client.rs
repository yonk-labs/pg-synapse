//! Minimal MCP (Model Context Protocol) HTTP client.
//!
//! Implements just enough of the spec at `modelcontextprotocol.io` for v0.1:
//! `initialize`, `tools/list`, and `tools/call`. The transport is JSON-RPC 2.0
//! over HTTP POST. Stdio and WebSocket transports are out of scope for the
//! kernel (those land as plugin crates per spec D11).
//!
//! Spec reference: design.md Section 7 ("Tool registry"), M2 plan Task 2.6.
//!
//! ## Error mapping
//!
//! All HTTP, JSON-RPC, and transport-level failures surface as
//! [`ToolError::Mcp`] with a human-readable detail. Tool-level execution
//! failures returned by the server come back as [`ToolError::Execution`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Tool;
use crate::error::ToolError;
use crate::types::{ToolCtx, ToolOutput, ToolSchema};

/// Server-reported info from `initialize`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct McpServerInfo {
    /// Human-readable server name.
    pub name: String,
    /// Server semver string.
    pub version: String,
}

/// One tool exposed by the MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    /// Tool name as advertised by the server.
    pub name: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for the tool's input. Per MCP spec the field is
    /// `inputSchema` (camelCase) on the wire.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// HTTP-transport client for an MCP server.
///
/// Cheap to share via `Arc`. One client is created per server URL and shared
/// across all tools the server exposes.
pub struct McpClient {
    http: Client,
    server_url: String,
    server_info: McpServerInfo,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server_url", &self.server_url)
            .field("server_info", &self.server_info)
            .finish()
    }
}

impl McpClient {
    /// Connect to an MCP server: HTTP `initialize` handshake.
    pub async fn connect(server_url: &str) -> Result<Self, ToolError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::Mcp(format!("http client build failed: {e}")))?;

        let mut client = Self {
            http,
            server_url: server_url.to_string(),
            server_info: McpServerInfo::default(),
        };
        client.server_info = client.initialize().await?;
        Ok(client)
    }

    /// Return the server's reported name + version.
    pub fn server_info(&self) -> &McpServerInfo {
        &self.server_info
    }

    /// MCP `initialize` method.
    async fn initialize(&self) -> Result<McpServerInfo, ToolError> {
        let params = json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "pg-synapse-core", "version": env!("CARGO_PKG_VERSION") }
        });
        let result = self.rpc("initialize", params).await?;
        // Some servers return `serverInfo`, some return the fields at the top
        // level. Try the spec-blessed `serverInfo` first; fall through to the
        // raw object otherwise.
        if let Some(info) = result.get("serverInfo") {
            let info: McpServerInfo = serde_json::from_value(info.clone())
                .map_err(|e| ToolError::Mcp(format!("invalid serverInfo: {e}")))?;
            return Ok(info);
        }
        serde_json::from_value(result)
            .map_err(|e| ToolError::Mcp(format!("invalid initialize result: {e}")))
    }

    /// MCP `tools/list` method.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, ToolError> {
        let result = self.rpc("tools/list", Value::Null).await?;
        let tools_val = result
            .get("tools")
            .cloned()
            .ok_or_else(|| ToolError::Mcp("tools/list response missing 'tools' field".into()))?;
        serde_json::from_value(tools_val)
            .map_err(|e| ToolError::Mcp(format!("invalid tools/list payload: {e}")))
    }

    /// MCP `tools/call` method. Returns the tool's output, mapped to a
    /// [`ToolOutput`].
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let params = json!({ "name": name, "arguments": args });
        let result = self.rpc("tools/call", params).await?;

        // MCP returns:
        //   { "content": [ { "type": "text", "text": "..." }, ... ],
        //     "isError": false }
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let reason = extract_text(&result).unwrap_or_else(|| "tool reported error".into());
            return Err(ToolError::Execution {
                name: name.to_string(),
                reason,
            });
        }
        if let Some(text) = extract_text(&result) {
            return Ok(ToolOutput::text(text));
        }
        // No textual content; return the raw `content` array as JSON, if any.
        if let Some(content) = result.get("content") {
            return Ok(ToolOutput::json(content.clone()));
        }
        Ok(ToolOutput::Empty)
    }

    /// One JSON-RPC 2.0 call against the MCP server.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": next_id(),
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.server_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::Mcp(format!("transport error calling {method}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::Mcp(format!(
                "http status {} from {}",
                status.as_u16(),
                method
            )));
        }
        let envelope: JsonRpcResponse = resp
            .json()
            .await
            .map_err(|e| ToolError::Mcp(format!("invalid JSON-RPC envelope: {e}")))?;
        if let Some(err) = envelope.error {
            return Err(ToolError::Mcp(format!(
                "jsonrpc error {} from {}: {}",
                err.code, method, err.message
            )));
        }
        envelope
            .result
            .ok_or_else(|| ToolError::Mcp(format!("jsonrpc response missing result for {method}")))
    }
}

fn extract_text(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    let mut out = String::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[allow(dead_code)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[allow(dead_code)]
    data: Option<Value>,
}

/// A [`Tool`] backed by an MCP server.
///
/// Constructed indirectly via [`crate::ToolRegistry::add_mcp`]; one tool
/// instance per remote tool the server exposes. All instances share the same
/// [`McpClient`] via `Arc`.
pub struct McpTool {
    client: Arc<McpClient>,
    name: String,
    schema: ToolSchema,
}

impl McpTool {
    /// Build an [`McpTool`] explicitly. Most callers should reach this via
    /// `ToolRegistry::add_mcp` instead.
    pub fn new(client: Arc<McpClient>, name: String, schema: ToolSchema) -> Self {
        Self {
            client,
            name,
            schema,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &ToolSchema {
        &self.schema
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.client.call_tool(&self.name, input).await
    }
}
