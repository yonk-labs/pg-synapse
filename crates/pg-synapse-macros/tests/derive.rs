//! Integration tests for `#[derive(Tool)]`.
//!
//! Each test exercises a different attribute and behavior path of the macro.

use pg_synapse_core::Tool as ToolTrait;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_macros::Tool;
use schemars::JsonSchema;
use serde::Deserialize;

/// Basic positive case: explicit name + description, struct round-trip.
#[derive(Tool, JsonSchema, Deserialize)]
#[tool(name = "echo", description = "Echo the input back")]
struct Echo {
    /// Text to echo.
    text: String,
}

impl Echo {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(self.text))
    }
}

#[tokio::test]
async fn echo_via_derive() {
    let tool = Echo {
        text: "ignored-template".into(),
    };
    assert_eq!(ToolTrait::name(&tool), "echo");
    assert_eq!(Echo::TOOL_NAME, "echo");
    assert_eq!(Echo::TOOL_DESCRIPTION, "Echo the input back");
    let out = ToolTrait::run(
        &tool,
        serde_json::json!({"text": "world"}),
        &ToolCtx::default(),
    )
    .await
    .unwrap();
    match out {
        ToolOutput::Text(s) => assert_eq!(s, "world"),
        other => panic!("expected Text output, got {:?}", other),
    }
}

/// No `name` attribute, no `description`: macro defaults both.
#[derive(Tool, JsonSchema, Deserialize)]
struct PingPong {}

impl PingPong {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Empty)
    }
}

#[tokio::test]
async fn missing_name_defaults_to_struct_name_lowercase() {
    let t = PingPong {};
    assert_eq!(ToolTrait::name(&t), "pingpong");
    assert_eq!(PingPong::TOOL_DESCRIPTION, "");
}

/// Bad input must surface as `ToolError::InvalidInput { name, reason }`.
#[derive(Tool, JsonSchema, Deserialize)]
#[tool(name = "needs_int")]
struct NeedsInt {
    /// Required integer field.
    n: i64,
}

impl NeedsInt {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Json(serde_json::json!({"n": self.n})))
    }
}

#[tokio::test]
async fn invalid_input_returns_invalid_input_error() {
    let t = NeedsInt { n: 0 };
    // Wrong type: "n" is a string, not an integer.
    let err = ToolTrait::run(
        &t,
        serde_json::json!({"n": "not-a-number"}),
        &ToolCtx::default(),
    )
    .await
    .expect_err("should reject bad input");
    match err {
        ToolError::InvalidInput { name, reason } => {
            assert_eq!(name, "needs_int");
            assert!(
                !reason.is_empty(),
                "reason must carry serde's message; got empty"
            );
        }
        other => panic!("expected InvalidInput, got {:?}", other),
    }
}

/// `schema()` must be stable: same pointer across calls (cached in OnceLock).
#[tokio::test]
async fn schema_is_cached_across_calls() {
    let t = Echo { text: "x".into() };
    let s1 = ToolTrait::schema(&t) as *const _;
    let s2 = ToolTrait::schema(&t) as *const _;
    assert_eq!(s1, s2, "schema() must return a stable reference");
}

/// The generated `Tool::run` must pass the caller `ToolCtx` through to the
/// user's inherent `run` method.
#[derive(Tool, JsonSchema, Deserialize)]
#[tool(name = "ctx_peek")]
struct CtxPeek {}

impl CtxPeek {
    async fn run(self, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let role = ctx.caller_role.clone().unwrap_or_default();
        Ok(ToolOutput::text(role))
    }
}

#[tokio::test]
async fn ctx_is_forwarded_to_user_run() {
    let t = CtxPeek {};
    let ctx = ToolCtx {
        execution_id: uuid::Uuid::nil(),
        caller_role: Some("auditor".into()),
        agent_name: None,
    };
    let out = ToolTrait::run(&t, serde_json::json!({}), &ctx)
        .await
        .unwrap();
    match out {
        ToolOutput::Text(s) => assert_eq!(s, "auditor"),
        other => panic!("expected Text, got {:?}", other),
    }
}
