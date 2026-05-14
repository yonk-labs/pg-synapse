//! The [`Tool`] trait and the [`ToolRegistry`] that holds them.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::ToolError;
use crate::types::{ToolCtx, ToolOutput, ToolSchema};

pub mod mcp_client;
pub use mcp_client::{McpClient, McpServerInfo, McpTool, McpToolDef};

/// A function the agent can call. Implementations describe their input via
/// [`Tool::schema`] (used to drive function-calling on the LLM side) and run
/// asynchronously.
///
/// Implementations must be `Send + Sync`; `ToolRegistry` stores them as
/// `Arc<dyn Tool>`.
///
/// ## Example
///
/// ```
/// use async_trait::async_trait;
/// use pg_synapse_core::{Tool, ToolError};
/// use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
///
/// struct EchoTool {
///     schema: ToolSchema,
/// }
///
/// #[async_trait]
/// impl Tool for EchoTool {
///     fn name(&self) -> &str { "echo" }
///     fn schema(&self) -> &ToolSchema { &self.schema }
///     async fn run(
///         &self,
///         input: serde_json::Value,
///         _ctx: &ToolCtx,
///     ) -> Result<ToolOutput, ToolError> {
///         Ok(ToolOutput::json(input))
///     }
/// }
/// ```
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name; must be unique within a registry.
    fn name(&self) -> &str;

    /// JSON Schema for the tool's input. Borrowed so callers can clone or
    /// pass by reference to the LLM provider.
    fn schema(&self) -> &ToolSchema;

    /// Execute the tool. `input` is the JSON-decoded argument object; tools
    /// must validate it against their schema and return
    /// [`ToolError::InvalidInput`] on mismatch.
    async fn run(&self, input: serde_json::Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError>;
}

/// In-memory map of tool name to `Arc<dyn Tool>`.
///
/// Cheap to clone (each clone shares the same underlying tool instances).
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a tool. Replaces any prior tool registered under the same name.
    /// Returns `&mut self` for chaining.
    pub fn add(&mut self, tool: impl Tool + 'static) -> &mut Self {
        self.tools.insert(tool.name().to_owned(), Arc::new(tool));
        self
    }

    /// Insert a tool already wrapped in an `Arc`. Used when copying shared
    /// instances out of a parent registry into a per-agent sub-registry (see
    /// `Runtime::resolve_tools`). Replaces any prior tool of the same name.
    pub fn add_arc(&mut self, name: impl Into<String>, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(name.into(), tool);
        self
    }

    /// True when a tool under `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// List the names of every registered tool.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Register every tool an MCP server advertises.
    ///
    /// Performs the MCP `initialize` handshake against `server_url`, calls
    /// `tools/list`, and inserts one [`McpTool`] per advertised tool. All
    /// inserted tools share a single underlying [`McpClient`] via `Arc`. If
    /// a tool with the same name already exists in the registry it is
    /// replaced.
    ///
    /// Returns `&mut self` for chaining.
    pub async fn add_mcp(&mut self, server_url: &str) -> Result<&mut Self, ToolError> {
        let client = Arc::new(McpClient::connect(server_url).await?);
        let defs = client.list_tools().await?;
        for def in defs {
            let schema = ToolSchema::from_json_value(def.input_schema).map_err(|e| {
                ToolError::Mcp(format!("invalid schema for tool '{}': {}", def.name, e))
            })?;
            let tool = McpTool::new(client.clone(), def.name.clone(), schema);
            self.tools.insert(def.name, Arc::new(tool));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    struct NoopTool;

    #[async_trait]
    impl Tool for NoopTool {
        fn name(&self) -> &str {
            "noop"
        }
        fn schema(&self) -> &ToolSchema {
            static S: OnceLock<ToolSchema> = OnceLock::new();
            S.get_or_init(ToolSchema::default)
        }
        async fn run(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolCtx,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Empty)
        }
    }

    #[tokio::test]
    async fn registry_add_and_get() {
        let mut r = ToolRegistry::new();
        r.add(NoopTool);
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
        let t = r.get("noop").expect("tool present");
        let out = t
            .run(serde_json::Value::Null, &ToolCtx::default())
            .await
            .unwrap();
        assert!(matches!(out, ToolOutput::Empty));
    }

    #[tokio::test]
    async fn registry_names_lists_inserted_tools() {
        let mut r = ToolRegistry::new();
        r.add(NoopTool);
        let names = r.names();
        assert_eq!(names, vec!["noop"]);
    }

    #[tokio::test]
    async fn registry_add_arc_shares_instance() {
        let mut src = ToolRegistry::new();
        src.add(NoopTool);
        let tool = src.get("noop").expect("present");
        let mut sink = ToolRegistry::new();
        sink.add_arc("renamed", tool.clone());
        assert!(sink.contains("renamed"));
        let out = sink
            .get("renamed")
            .unwrap()
            .run(serde_json::Value::Null, &ToolCtx::default())
            .await
            .unwrap();
        assert!(matches!(out, ToolOutput::Empty));
    }

    #[test]
    fn registry_contains_reflects_membership() {
        let mut r = ToolRegistry::new();
        assert!(!r.contains("noop"));
        r.add(NoopTool);
        assert!(r.contains("noop"));
        assert!(!r.contains("missing"));
    }

    #[tokio::test]
    async fn add_mcp_with_unreachable_url_returns_typed_mcp_error() {
        // Loopback non-listening port: 1 (privileged + never listening).
        // We bind to 127.0.0.1:0 and immediately drop the listener to free
        // the port, then point the client at the freed port so the connect
        // refuses cleanly without depending on a fixed unused port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}");

        let mut r = ToolRegistry::new();
        let result = r.add_mcp(&url).await;
        match result {
            Err(ToolError::Mcp(_)) => {}
            other => panic!(
                "expected ToolError::Mcp from unreachable URL, got {:?}",
                other.map(|_| "Ok")
            ),
        }
    }
}
