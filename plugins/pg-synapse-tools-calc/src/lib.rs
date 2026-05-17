//! Calculator tool plugin for pg_synapse: `calculator`.
//!
//! Supports four binary operations: add, sub, mul, div.
//! Division by zero returns a [`ToolError::Execution`].
//!
//! ## Arg-alias leniency
//!
//! `op` accepts alias `operation`. `a` accepts alias `x`. `b` accepts alias `y`.
//! An additional `expression` field is accepted but silently ignored (LLMs
//! sometimes emit it alongside the structured fields).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::OnceLock;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use schemars::JsonSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

// ---------------------------------------------------------------------------
// Schema builder helper
// ---------------------------------------------------------------------------

fn build_schema<T: JsonSchema>() -> ToolSchema {
    let root = schema_for!(T);
    let val = serde_json::to_value(&root).expect("schemars output is always valid JSON");
    ToolSchema::from_json_value(val).expect("schemars schema is always a valid object")
}

// ---------------------------------------------------------------------------
// Input struct with lenient aliases
// ---------------------------------------------------------------------------

/// Input schema for `calculator`.
///
/// Canonical fields: `op`, `a`, `b`.
/// Accepted aliases: `operation` for `op`; `x` for `a`; `y` for `b`.
/// An optional `expression` field is accepted and ignored.
#[derive(Deserialize, JsonSchema, Debug)]
struct CalcInput {
    /// Operation to perform: "add", "sub", "mul", or "div".
    #[serde(alias = "operation")]
    op: String,

    /// First operand.
    #[serde(alias = "x")]
    a: f64,

    /// Second operand.
    #[serde(alias = "y")]
    b: f64,

    /// Optional expression string (accepted but ignored for leniency with models
    /// that emit a human-readable expression alongside structured args).
    #[allow(dead_code)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expression: Option<String>,
}

// ---------------------------------------------------------------------------
// calculator tool
// ---------------------------------------------------------------------------

/// Tool: perform a binary arithmetic operation.
///
/// Accepts `op` (add|sub|mul|div), `a`, `b`. Returns `{"result": <number>}`.
pub struct CalculatorTool {
    schema: OnceLock<ToolSchema>,
}

impl CalculatorTool {
    fn new() -> Self {
        Self {
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<CalcInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: CalcInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "calculator".into(),
                reason: e.to_string(),
            })?;

        debug!("calculator: op={} a={} b={}", args.op, args.a, args.b);

        let result = match args.op.to_lowercase().as_str() {
            "add" => args.a + args.b,
            "sub" => args.a - args.b,
            "mul" => args.a * args.b,
            "div" => {
                if args.b == 0.0 {
                    return Err(ToolError::Execution {
                        name: "calculator".into(),
                        reason: "division by zero".into(),
                    });
                }
                args.a / args.b
            }
            other => {
                return Err(ToolError::InvalidInput {
                    name: "calculator".into(),
                    reason: format!("unknown op '{other}': must be one of add, sub, mul, div"),
                });
            }
        };

        Ok(ToolOutput::Json(serde_json::json!({ "result": result })))
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Plugin that registers `calculator` into a host [`Registry`].
pub struct CalcToolsPlugin;

impl CalcToolsPlugin {
    /// Create the plugin. No configuration required.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CalcToolsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CalcToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-calc"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry
            .tools
            .add_arc("calculator", std::sync::Arc::new(CalculatorTool::new()));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pg_synapse_core::types::ToolCtx;

    fn ctx() -> ToolCtx {
        ToolCtx::default()
    }

    #[tokio::test]
    async fn add_two_numbers() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(serde_json::json!({"op": "add", "a": 3.0, "b": 4.0}), &ctx())
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 7.0})));
    }

    #[tokio::test]
    async fn sub_two_numbers() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(
                serde_json::json!({"op": "sub", "a": 10.0, "b": 3.0}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 7.0})));
    }

    #[tokio::test]
    async fn mul_two_numbers() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(serde_json::json!({"op": "mul", "a": 6.0, "b": 7.0}), &ctx())
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 42.0})));
    }

    #[tokio::test]
    async fn div_two_numbers() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(
                serde_json::json!({"op": "div", "a": 42.0, "b": 6.0}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 7.0})));
    }

    #[tokio::test]
    async fn div_by_zero_returns_tool_error() {
        let tool = CalculatorTool::new();
        let err = tool
            .run(serde_json::json!({"op": "div", "a": 1.0, "b": 0.0}), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution { .. }));
    }

    #[tokio::test]
    async fn accepts_op_alias_operation() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(
                serde_json::json!({"operation": "add", "a": 1.0, "b": 1.0}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 2.0})));
    }

    #[tokio::test]
    async fn accepts_x_y_aliases() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(serde_json::json!({"op": "mul", "x": 3.0, "y": 5.0}), &ctx())
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 15.0})));
    }

    #[tokio::test]
    async fn ignores_expression_field() {
        let tool = CalculatorTool::new();
        let out = tool
            .run(
                serde_json::json!({"op": "add", "a": 2.0, "b": 3.0, "expression": "2 + 3"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out, ToolOutput::Json(serde_json::json!({"result": 5.0})));
    }

    #[tokio::test]
    async fn unknown_op_returns_invalid_input() {
        let tool = CalculatorTool::new();
        let err = tool
            .run(serde_json::json!({"op": "pow", "a": 2.0, "b": 3.0}), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }
}
