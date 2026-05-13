//! Tool-side data types: JSON-Schema wrapper, output envelope, invocation context.

use schemars::schema::RootSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// JSON Schema describing a tool's input.
///
/// Newtype over [`schemars::schema::RootSchema`] so the kernel can change the
/// schema dialect later without rippling through tool authors. v0.1 uses
/// `schemars` defaults (draft 2020-12 emitter).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolSchema(
    /// Underlying JSON Schema document.
    pub RootSchema,
);

impl ToolSchema {
    /// Wrap an existing `RootSchema` value.
    pub fn new(schema: RootSchema) -> Self {
        Self(schema)
    }

    /// Borrow the underlying schema.
    pub fn as_root_schema(&self) -> &RootSchema {
        &self.0
    }

    /// Consume self and return the underlying schema.
    pub fn into_inner(self) -> RootSchema {
        self.0
    }
}

/// What a tool returned to the executor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum ToolOutput {
    /// Plain text output, fed back to the LLM as-is.
    Text(String),
    /// Structured JSON output, fed back as a JSON tool-result message.
    Json(serde_json::Value),
    /// Tool ran successfully but has no content (side-effect-only).
    Empty,
}

impl ToolOutput {
    /// Convenience constructor for text output.
    pub fn text(s: impl Into<String>) -> Self {
        ToolOutput::Text(s.into())
    }

    /// Convenience constructor for JSON output.
    pub fn json(v: serde_json::Value) -> Self {
        ToolOutput::Json(v)
    }
}

/// Context passed to every [`crate::Tool::run`] call.
///
/// Carries the execution ID, the calling Postgres role (for audit), and the
/// agent name (for trace correlation). Tools that need richer state should
/// capture it via their own constructor; `ToolCtx` is intentionally narrow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolCtx {
    /// The execution that issued this tool call.
    pub execution_id: Uuid,
    /// Postgres role that invoked the agent (for `executions.caller_role`).
    pub caller_role: Option<String>,
    /// Agent name that issued this tool call.
    pub agent_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_text_constructor() {
        let o = ToolOutput::text("hello");
        assert_eq!(o, ToolOutput::Text("hello".into()));
    }

    #[test]
    fn tool_output_roundtrips() {
        let cases = vec![
            ToolOutput::Text("x".into()),
            ToolOutput::Json(serde_json::json!({"a": 1})),
            ToolOutput::Empty,
        ];
        for c in cases {
            let s = serde_json::to_string(&c).unwrap();
            let back: ToolOutput = serde_json::from_str(&s).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn tool_ctx_default_is_empty() {
        let c = ToolCtx::default();
        assert_eq!(c.execution_id, Uuid::nil());
        assert!(c.caller_role.is_none());
        assert!(c.agent_name.is_none());
    }

    #[test]
    fn tool_ctx_debug_format_includes_fields() {
        let c = ToolCtx {
            execution_id: Uuid::nil(),
            caller_role: Some("admin".into()),
            agent_name: Some("a1".into()),
        };
        let s = format!("{:?}", c);
        assert!(s.contains("admin"));
        assert!(s.contains("a1"));
    }

    #[test]
    fn tool_schema_default_is_serializable() {
        let s = ToolSchema::default();
        let _ = serde_json::to_string(&s).unwrap();
    }
}
