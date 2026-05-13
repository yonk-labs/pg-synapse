//! [`crate::Tool`] mock that returns a fixed response.

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::Tool;
use crate::types::{ToolCtx, ToolOutput, ToolSchema};

/// A trivial [`Tool`] that returns a pre-canned [`ToolOutput`] for every call.
///
/// Useful for [`crate::ToolRegistry`] tests, executor tests, and any place
/// that needs to wire a tool into a context without authoring a real one.
///
/// ## Example
///
/// ```
/// use pg_synapse_core::testing::MockTool;
/// use pg_synapse_core::types::{ToolCtx, ToolOutput};
/// use pg_synapse_core::Tool;
///
/// # tokio_test::block_on(async {
/// let t = MockTool::new("echo", ToolOutput::text("hi"));
/// let out = t.run(serde_json::Value::Null, &ToolCtx::default()).await.unwrap();
/// assert!(matches!(out, ToolOutput::Text(s) if s == "hi"));
/// # });
/// ```
pub struct MockTool {
    name: String,
    schema: ToolSchema,
    response: ToolOutput,
}

impl MockTool {
    /// Construct a tool with the given `name` that always returns `response`.
    pub fn new(name: impl Into<String>, response: ToolOutput) -> Self {
        Self {
            name: name.into(),
            schema: ToolSchema::default(),
            response,
        }
    }

    /// Replace the tool's schema. Useful when a test needs a non-empty schema.
    pub fn with_schema(mut self, schema: ToolSchema) -> Self {
        self.schema = schema;
        self
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &ToolSchema {
        &self.schema
    }

    async fn run(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_configured_response() {
        let t = MockTool::new("echo", ToolOutput::text("hello"));
        let out = t
            .run(serde_json::Value::Null, &ToolCtx::default())
            .await
            .unwrap();
        match out {
            ToolOutput::Text(s) => assert_eq!(s, "hello"),
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn json_response_passes_through() {
        let t = MockTool::new("j", ToolOutput::Json(serde_json::json!({"x": 1})));
        let out = t
            .run(serde_json::Value::Null, &ToolCtx::default())
            .await
            .unwrap();
        match out {
            ToolOutput::Json(v) => assert_eq!(v, serde_json::json!({"x": 1})),
            _ => panic!("expected json"),
        }
    }

    #[test]
    fn name_reflects_constructor() {
        let t = MockTool::new("mytool", ToolOutput::Empty);
        assert_eq!(t.name(), "mytool");
    }
}
