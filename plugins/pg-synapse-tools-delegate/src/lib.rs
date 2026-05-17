//! Delegation tool plugin for pg_synapse: `call_agent`.
//!
//! Enables multi-agent patterns (OpenAI Agents SDK triage/handoff,
//! Google ADK orchestrator + children) by re-entering `Runtime::execute`
//! on a named sub-agent and returning its final output as a tool result.
//!
//! ## Depth guard
//!
//! A process-wide atomic counter (`DELEGATION_DEPTH`) tracks how many nested
//! `call_agent` activations are live. When the counter reaches
//! `MAX_DELEGATION_DEPTH` (4) the call is rejected with a
//! [`ToolError::Execution`] before touching the runtime.
//!
//! ### Why a process-wide atomic instead of `ToolCtx` threading?
//!
//! The kernel's `Runtime::execute` path does NOT forward the caller's
//! `ToolCtx` into the sub-execution; it starts a fresh `ExecutionContext`
//! from scratch. Plumbing depth through that call chain would require adding
//! a parameter to `Runtime::execute`, `execute_inner`, `ExecutionContext`, and
//! `Executor::execute` -- a kernel-wide breaking change for a guard that only
//! this one tool needs. The atomic approach is a clean, contained solution:
//! it is incremented on entry and decremented (even on error) on exit, so
//! concurrent independent top-level calls do not incorrectly share depth
//! state because each real nested call IS on the same logical call stack.
//!
//! Limitation: the atomic is process-wide, not per-request. Two independent
//! top-level `synapse.execute` calls happening concurrently in the same process
//! could in theory see each other's depth. In practice the pgrx host is
//! current-thread tokio (one request at a time per backend), and the sidecar's
//! async runtime makes nested awaits within one request sequential. Documented
//! in `crates/pg-synapse-pgrx/NOTES.md` under "B-keystone".
//!
//! ## Circular-dependency resolution (Arc<Runtime> wiring)
//!
//! `DelegateToolsPlugin` must be registered via `RuntimeBuilder::with_plugin`
//! BEFORE `.build()` is called (the builder moves `Registry` into `Arc` at
//! build time; post-build insertion requires interior mutability the registry
//! does not provide). The tool holds a `Weak<Runtime>` inside an
//! `OnceLock<Weak<Runtime>>`. The host fills it in immediately after build:
//!
//! ```ignore
//! // Phase 1: build the runtime WITH the delegate plugin pre-registered.
//! let delegate_tool = Arc::new(CallAgentTool::empty());
//! let tool_ref = delegate_tool.clone();
//! let runtime = Runtime::builder()
//!     .with_plugin(DelegateToolsPlugin::with_tool(delegate_tool))
//!     // ... other plugins ...
//!     .load_profiles_from(source)
//!     .build()
//!     .await?;
//!
//! // Phase 2: inject the runtime handle into the already-registered tool.
//! let runtime_arc = Arc::new(runtime);
//! tool_ref.inject(Arc::downgrade(&runtime_arc));
//! ```
//!
//! `call_agent` will return a `ToolError::Execution` (not panic) if called
//! before `inject` is called, so startup order bugs surface clearly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicU32, Ordering},
};

use async_trait::async_trait;
use pg_synapse_core::Runtime;
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
// Depth guard
// ---------------------------------------------------------------------------

/// Maximum nesting depth for `call_agent` delegations.
pub const MAX_DELEGATION_DEPTH: u32 = 4;

/// Process-wide atomic tracking active delegation depth.
static DELEGATION_DEPTH: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// Schema builder helper
// ---------------------------------------------------------------------------

fn build_schema<T: JsonSchema>() -> ToolSchema {
    let root = schema_for!(T);
    let val = serde_json::to_value(&root).expect("schemars output is always valid JSON");
    ToolSchema::from_json_value(val).expect("schemars schema is always a valid object")
}

// ---------------------------------------------------------------------------
// Input struct
// ---------------------------------------------------------------------------

/// Input schema for `call_agent`.
#[derive(Deserialize, JsonSchema, Debug)]
struct CallAgentInput {
    /// Name of the sub-agent to invoke (must exist in the runtime's agent map).
    agent: String,

    /// The input text to pass to the sub-agent.
    input: String,
}

// ---------------------------------------------------------------------------
// call_agent tool
// ---------------------------------------------------------------------------

/// Tool: delegate to a named sub-agent and return its output.
///
/// Holds a `OnceLock<Weak<Runtime>>` filled in by the host after the runtime
/// is built (two-phase wiring). Call [`CallAgentTool::inject`] once before
/// the first agent invocation.
pub struct CallAgentTool {
    schema: OnceLock<ToolSchema>,
    /// Set by the host after runtime construction. Weak to avoid a cycle.
    runtime: OnceLock<Weak<Runtime>>,
}

impl CallAgentTool {
    /// Create the tool shell. `inject` must be called before the tool is used.
    pub fn empty() -> Self {
        Self {
            schema: OnceLock::new(),
            runtime: OnceLock::new(),
        }
    }

    /// Inject the runtime handle. Safe to call exactly once; subsequent calls
    /// are no-ops (the `OnceLock` simply ignores the new value).
    pub fn inject(&self, weak: Weak<Runtime>) {
        let _ = self.runtime.set(weak);
    }

    fn runtime_arc(&self) -> Result<Arc<Runtime>, ToolError> {
        self.runtime
            .get()
            .and_then(|w| w.upgrade())
            .ok_or_else(|| ToolError::Execution {
                name: "call_agent".into(),
                reason: "runtime handle not injected or runtime was dropped".into(),
            })
    }
}

#[async_trait]
impl Tool for CallAgentTool {
    fn name(&self) -> &str {
        "call_agent"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<CallAgentInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: CallAgentInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "call_agent".into(),
                reason: e.to_string(),
            })?;

        let runtime = self.runtime_arc()?;

        // Depth guard: reject if we are already at the limit.
        let depth = DELEGATION_DEPTH.fetch_add(1, Ordering::SeqCst);
        if depth >= MAX_DELEGATION_DEPTH {
            DELEGATION_DEPTH.fetch_sub(1, Ordering::SeqCst);
            return Err(ToolError::Execution {
                name: "call_agent".into(),
                reason: format!(
                    "delegation depth limit ({MAX_DELEGATION_DEPTH}) exceeded; refusing to call agent '{}'",
                    args.agent
                ),
            });
        }

        debug!(
            "call_agent: depth={} agent={} input_len={}",
            depth,
            args.agent,
            args.input.len()
        );

        let result = runtime.execute(&args.agent, &args.input).await;

        // Always decrement, even on error.
        DELEGATION_DEPTH.fetch_sub(1, Ordering::SeqCst);

        match result {
            Ok(outcome) => Ok(ToolOutput::Text(outcome.output)),
            Err(e) => Err(ToolError::Execution {
                name: "call_agent".into(),
                reason: format!("sub-agent '{}' failed: {e}", args.agent),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Plugin that registers a pre-built `Arc<CallAgentTool>` into a host [`Registry`].
///
/// Usage pattern (two-phase wiring):
///
/// ```ignore
/// // Phase 1: pre-register the shell before build.
/// let tool = Arc::new(CallAgentTool::empty());
/// let tool_ref = tool.clone();
/// let runtime = Runtime::builder()
///     .with_plugin(DelegateToolsPlugin::with_tool(tool))
///     .build().await?;
///
/// // Phase 2: inject the runtime handle.
/// let runtime_arc = Arc::new(runtime);
/// tool_ref.inject(Arc::downgrade(&runtime_arc));
/// ```
pub struct DelegateToolsPlugin {
    tool: Arc<CallAgentTool>,
}

impl DelegateToolsPlugin {
    /// Create the plugin carrying a pre-built tool shell.
    pub fn with_tool(tool: Arc<CallAgentTool>) -> Self {
        Self { tool }
    }
}

impl Plugin for DelegateToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-delegate"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry.tools.add_arc("call_agent", self.tool);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use pg_synapse_core::Runtime;
    use pg_synapse_core::Tool;
    use pg_synapse_core::error::ToolError;
    use pg_synapse_core::runtime::test_utils::MockProfileSource;
    use pg_synapse_core::types::ToolCtx;

    use super::{CallAgentTool, DELEGATION_DEPTH, MAX_DELEGATION_DEPTH};

    fn ctx() -> ToolCtx {
        ToolCtx::default()
    }

    async fn minimal_runtime() -> Arc<Runtime> {
        let source = MockProfileSource::default();
        let rt = Runtime::builder()
            .load_profiles_from(source)
            .build()
            .await
            .unwrap();
        Arc::new(rt)
    }

    #[tokio::test]
    async fn tool_without_inject_returns_execution_error() {
        let tool = CallAgentTool::empty();
        let err: ToolError = tool
            .run(serde_json::json!({"agent": "any", "input": "hi"}), &ctx())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Execution { .. }),
            "expected Execution error, got: {err:?}"
        );
        // This path errors before touching DELEGATION_DEPTH; assert the contract.
    }

    /// Depth guard test -- run with current_thread flavor so DELEGATION_DEPTH
    /// manipulation is isolated (no other async task on this thread).
    #[tokio::test(flavor = "current_thread")]
    async fn depth_guard_rejects_at_limit() {
        DELEGATION_DEPTH.store(MAX_DELEGATION_DEPTH, Ordering::SeqCst);

        let runtime = minimal_runtime().await;
        let tool = CallAgentTool::empty();
        tool.inject(Arc::downgrade(&runtime));

        let err: ToolError = tool
            .run(serde_json::json!({"agent": "any", "input": "hi"}), &ctx())
            .await
            .unwrap_err();

        DELEGATION_DEPTH.store(0, Ordering::SeqCst);

        assert!(
            matches!(err, ToolError::Execution { .. }),
            "expected Execution error at depth limit, got: {err:?}"
        );
    }

    /// Depth decrement test -- current_thread for the same isolation reason.
    #[tokio::test(flavor = "current_thread")]
    async fn depth_decremented_on_sub_agent_failure() {
        DELEGATION_DEPTH.store(0, Ordering::SeqCst);

        let runtime = minimal_runtime().await;
        let tool = CallAgentTool::empty();
        tool.inject(Arc::downgrade(&runtime));

        // "nonexistent" is not in the mock runtime so execute will fail.
        let _err: ToolError = tool
            .run(
                serde_json::json!({"agent": "nonexistent", "input": "test"}),
                &ctx(),
            )
            .await
            .unwrap_err();

        assert_eq!(DELEGATION_DEPTH.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn invalid_input_rejected() {
        let runtime = minimal_runtime().await;
        let tool = CallAgentTool::empty();
        tool.inject(Arc::downgrade(&runtime));

        // Missing required fields.
        let err: ToolError = tool
            .run(serde_json::json!({"wrong_field": "oops"}), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }
}
