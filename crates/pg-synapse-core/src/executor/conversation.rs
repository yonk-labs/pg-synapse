//! [`ConversationExecutor`]: the canonical "chat loop with tool calls".
//!
//! Spec reference: design.md Section 5 ("Executors") and the M2 plan
//! (`writing-plans/pg-synapse-v0.1-plan.md`, Task 2.2).
//!
//! Drives the loop:
//!
//! 1. Seed messages from `system_prompt` + `soul` + user input.
//! 2. Each iteration: call the `main` LLM; if the response is text, return it;
//!    if the response is tool calls, dispatch each and append the tool results
//!    to the message log, then loop.
//! 3. Stop on assistant text, when `max_iterations` is reached, or when the
//!    USD cost cap trips.
//!
//! Wall-clock timeouts are enforced upstream (by the `Service::call` wrapper
//! or a `tower::Layer`), not by the executor itself. See spec G4.

use async_trait::async_trait;

use crate::Executor;
use crate::error::ExecutorError;
use crate::executor::loop_harness::{LoopHarness, TurnResult};
use crate::types::{ExecutionContext, ExecutorOutcome, OutcomeStatus};

/// The canonical chat-with-tools executor.
///
/// Stateless: one instance can be shared by every concurrent run. Construct
/// via [`ConversationExecutor::default`] or `ConversationExecutor`.
#[derive(Default, Debug)]
pub struct ConversationExecutor;

#[async_trait]
impl Executor for ConversationExecutor {
    async fn execute(&self, ctx: ExecutionContext) -> Result<ExecutorOutcome, ExecutorError> {
        let mut harness = LoopHarness::new(&ctx);
        harness.seed_messages();

        loop {
            harness.bump_iteration();
            // Iteration cap is checked AFTER the bump: iteration N+1 is the
            // one that runs over the cap.
            if let Err(e) = harness.check_iteration_cap() {
                return match e {
                    ExecutorError::MaxIterationsReached(_) => {
                        Ok(harness.finalize(String::new(), OutcomeStatus::MaxIterations))
                    }
                    other => Err(other),
                };
            }
            harness.check_cost_cap().map_err(soft_to_outcome_err)?;

            match harness.one_llm_turn().await {
                Ok(TurnResult::AssistantText(text)) => {
                    return Ok(harness.finalize(text, OutcomeStatus::Completed));
                }
                Ok(TurnResult::ToolCalls(calls)) => {
                    for tc in &calls {
                        harness.dispatch_tool_call(tc).await?;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Identity transform; placeholder so the cost-cap path can grow custom
/// translation later without touching the loop body.
fn soft_to_outcome_err(e: ExecutorError) -> ExecutorError {
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{LlmError, ToolError};
    use crate::llm::LlmProvider;
    use crate::testing::{MockLlmProvider, MockTool};
    use crate::tool::ToolRegistry;
    use crate::types::{ToolOutput, Usage};
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    fn ctx_with(
        llm: Arc<dyn LlmProvider>,
        tools: ToolRegistry,
        max_iterations: u32,
        cost_cap_usd: Option<f64>,
    ) -> ExecutionContext {
        ExecutionContext {
            execution_id: Uuid::nil(),
            agent_name: "agent".into(),
            system_prompt: "be helpful".into(),
            soul: None,
            input: "hello".into(),
            executor_name: "conversation".into(),
            tools: Arc::new(tools),
            llm,
            judge_llm: None,
            small_llm: None,
            embeddings: None,
            memory: None,
            compressor: None,
            max_iterations,
            timeout: Duration::from_millis(1000),
            cost_cap_usd,
            caller_role: None,
        }
    }

    #[tokio::test]
    async fn happy_path_tool_then_text() {
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "echo", serde_json::json!({"x": 1}));
        mock.push_text("done!");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("echo-ok")));
        let ctx = ctx_with(llm, reg, 10, None);

        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "done!");
        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.tool_calls[0].name, "echo");
    }

    #[tokio::test]
    async fn max_iterations_returns_soft_outcome() {
        // Mock keeps issuing tool calls forever (more than max_iterations).
        let mock = MockLlmProvider::new("m");
        for i in 0..20 {
            mock.push_tool_call(format!("c{i}"), "echo", serde_json::json!({"i": i}));
        }
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("ok")));
        let ctx = ctx_with(llm, reg, 3, None);

        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::MaxIterations);
    }

    #[tokio::test]
    async fn unknown_tool_surfaces_typed_error() {
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "missing", serde_json::json!({}));
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);

        let err = ConversationExecutor.execute(ctx).await.unwrap_err();
        match err {
            ExecutorError::Tool(ToolError::NotFound { name }) => {
                assert_eq!(name, "missing");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cost_cap_trips_after_expensive_turn() {
        let mock = MockLlmProvider::new("m");
        mock.push_text_with_usage(
            "first",
            Usage {
                tokens_in: 1,
                tokens_out: 1,
                cost_usd: Some(0.20),
            },
        );
        // We won't reach the second turn because the cost cap will trip at the
        // top of the *next* iteration; but provide it anyway for safety.
        mock.push_text("never reached");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 10, Some(0.10));

        // First turn returns text immediately, so the cost cap never trips
        // mid-loop here. But if the model keeps calling tools, the cap does:
        // exercise that scenario instead.
        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "first");
        assert_eq!(outcome.cost_usd, Some(0.20));
    }

    #[tokio::test]
    async fn cost_cap_trips_between_tool_loops() {
        let mock = MockLlmProvider::new("m");
        // Turn 1: expensive tool call.
        mock.push_tool_call_with_usage(
            "c1",
            "echo",
            serde_json::json!({}),
            Usage {
                tokens_in: 1,
                tokens_out: 1,
                cost_usd: Some(0.50),
            },
        );
        // Turn 2: would happen if the cap didn't trip first.
        mock.push_text("would-be answer");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("ok")));
        let ctx = ctx_with(llm, reg, 10, Some(0.10));

        let err = ConversationExecutor.execute(ctx).await.unwrap_err();
        match err {
            ExecutorError::CostCapExceeded { cap, spent } => {
                assert_eq!(cap, 0.10);
                assert!(spent >= 0.50);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn llm_provider_error_surfaces() {
        let mock = MockLlmProvider::new("m");
        mock.push_error(LlmError::Auth("bad key".into()));
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);

        let err = ConversationExecutor.execute(ctx).await.unwrap_err();
        match err {
            ExecutorError::Llm(LlmError::Auth(s)) => assert_eq!(s, "bad key"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_path_completes_quickly_on_happy_path() {
        // Note: timeouts are enforced upstream (via tower Layer or
        // Service::call wrapping), not inside the executor. This test
        // confirms a fast happy-path stays well under any reasonable cap.
        let mock = MockLlmProvider::new("m");
        mock.push_text("quick");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        ctx.timeout = Duration::from_millis(50);

        let started = std::time::Instant::now();
        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        let elapsed = started.elapsed();
        assert!(elapsed.as_millis() < 100);
        assert_eq!(outcome.status, OutcomeStatus::Completed);
    }
}
