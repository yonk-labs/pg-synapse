# Tool Authoring

A tool is a function the agent can call. There are three ways to write one,
from most control to least code.

## The `Tool` trait

Every path produces an implementation of `pg_synapse_core::Tool`:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name; must be unique within a registry.
    fn name(&self) -> &str;

    /// JSON Schema for the tool's input (drives LLM function calling).
    fn schema(&self) -> &ToolSchema;

    /// Execute. `input` is the JSON-decoded argument object.
    async fn run(&self, input: serde_json::Value, ctx: &ToolCtx)
        -> Result<ToolOutput, ToolError>;
}
```

`ToolSchema` is a newtype over `schemars::schema::RootSchema` (draft 2020-12
emitter). `ToolOutput` is an enum: `Text(String)` (fed back to the model
verbatim), `Json(serde_json::Value)` (fed back as a structured tool result),
or `Empty` (side-effect-only). `ToolCtx` is intentionally narrow:

```rust
pub struct ToolCtx {
    pub execution_id: Uuid,
    pub caller_role: Option<String>,
    pub agent_name: Option<String>,
}
```

`caller_role` is the Postgres role that invoked `synapse.execute(...)`. The
built-in `sql_query` / `sql_exec` tools forward it to the host's SQL executor,
which runs SPI as `CURRENT_USER` (not the wrapping function's definer role), so
the agent can never exceed the caller's grants. Tools needing richer state
capture it in their own constructor.

`ToolError` variants: `NotFound { name }`, `InvalidInput { name, reason }`,
`Execution { name, reason }`, `Timeout { name, timeout_ms }`, `Mcp(String)`.

## Path 1: manual `Tool` impl

Full control. You own schema construction and input parsing.

```rust
use async_trait::async_trait;
use std::sync::OnceLock;
use pg_synapse_core::{Tool, ToolError};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};

struct AddTool {
    schema: ToolSchema,
}

impl AddTool {
    fn new() -> Self {
        // Build any RootSchema you like; schemars::schema_for! is one way.
        Self { schema: ToolSchema::default() }
    }
}

#[async_trait]
impl Tool for AddTool {
    fn name(&self) -> &str { "add" }

    fn schema(&self) -> &ToolSchema { &self.schema }

    async fn run(&self, input: serde_json::Value, _ctx: &ToolCtx)
        -> Result<ToolOutput, ToolError>
    {
        let a = input.get("a").and_then(|v| v.as_i64()).ok_or_else(|| {
            ToolError::InvalidInput { name: "add".into(), reason: "missing a".into() }
        })?;
        let b = input.get("b").and_then(|v| v.as_i64()).ok_or_else(|| {
            ToolError::InvalidInput { name: "add".into(), reason: "missing b".into() }
        })?;
        Ok(ToolOutput::json(serde_json::json!({ "sum": a + b })))
    }
}
```

Reach for this when the input schema is dynamic, when you cannot derive
`schemars::JsonSchema` on your input type, or when `run` needs `&self` state
that is awkward to express as deserializable fields.

## Path 2: `#[derive(Tool)]`

The common path. Annotate a struct that derives `serde::Deserialize` and
`schemars::JsonSchema`; the macro generates `name()`, `schema()` (cached in a
`OnceLock`), and `run()` (which deserializes the input into your struct, maps
deserialization failures to `ToolError::InvalidInput`, then calls your
inherent `async fn run(self, ctx: &ToolCtx)`). It also emits `Self::TOOL_NAME`
and `Self::TOOL_DESCRIPTION` consts.

This is exactly how the HTTP tools plugin defines `http_get`
(`plugins/pg-synapse-tools-http/src/lib.rs`):

```rust
use std::collections::BTreeMap;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_macros::Tool as DeriveTool;
use schemars::JsonSchema;
use serde::Deserialize;

/// Fetch a URL via HTTP GET. Returns the response body as text.
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_get",
    description = "Fetch a URL via HTTP GET. Returns the response status and body as text."
)]
pub struct HttpGet {
    /// Absolute URL to fetch.
    pub url: String,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpGet {
    /// User-facing handler. The derive macro wraps this with input parsing.
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let mut req = http().get(&self.url);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "body": body }),
        ))
    }
}
```

`#[tool(name = ..., description = ...)]` is the only supported attribute; both
fields are optional (`name` defaults to the struct ident lowercased,
`description` to empty). Doc comments on fields flow into the JSON Schema via
`schemars`, so the model sees them. Register the tool through a `Plugin`
(see [plugin-development.md](./plugin-development.md)): the HTTP plugin builds
one template instance per tool and calls
`registry.tools.add_arc(HttpGet::TOOL_NAME.to_string(), Arc::new(getter))`.
The template fields are inert because `run` re-parses input from JSON on every
call.

## Path 3: MCP client

Borrow tools from any Model Context Protocol server over HTTP. One call wires
in every tool the server advertises:

```rust
use pg_synapse_core::tool::ToolRegistry;

let mut registry = ToolRegistry::new();
registry.add_mcp("http://localhost:9000").await?;
```

`add_mcp` performs the MCP `initialize` handshake against `server_url`, calls
`tools/list`, and inserts one `McpTool` per advertised tool. The transport is
JSON-RPC 2.0 over HTTP POST (stdio and WebSocket transports are out of scope
for v0.1). All inserted tools share a single underlying `McpClient` via `Arc`;
each `run` issues a `tools/call`. HTTP, JSON-RPC, and framing failures surface
as `ToolError::Mcp(String)`. A name collision replaces the existing tool.

## Which path when

Use **`#[derive(Tool)]`** for almost everything: it is the least code, the
schema stays in sync with the input type, and field doc comments become model
hints automatically. Drop to a **manual impl** when the schema must be dynamic,
the input type cannot derive `JsonSchema`, or `run` needs stateful `&self`
behavior. Use the **MCP client** when the capability already exists as an MCP
server and you would rather integrate than reimplement.
