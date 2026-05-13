//! The [`Tool`] trait and the [`ToolRegistry`] that holds them.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::ToolError;
use crate::types::{ToolCtx, ToolOutput, ToolSchema};

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

    /// Register an MCP server's exposed tools.
    ///
    /// **Not implemented in M1.** The MCP client wires up in M2; this stub
    /// returns [`ToolError::Mcp`] so callers get a typed error today and a
    /// real implementation later.
    pub async fn add_mcp(&mut self, _server_url: &str) -> Result<&mut Self, ToolError> {
        Err(ToolError::Mcp(
            "add_mcp() not yet wired in M1; lands in M2".into(),
        ))
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
    async fn add_mcp_returns_typed_error_in_m1() {
        let mut r = ToolRegistry::new();
        let result = r.add_mcp("http://example.invalid").await;
        match result {
            Err(ToolError::Mcp(_)) => {}
            other => panic!("expected ToolError::Mcp, got {:?}", other.map(|_| "Ok")),
        }
    }
}
