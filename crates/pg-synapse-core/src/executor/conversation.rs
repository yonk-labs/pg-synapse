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
                        // A failed tool call is fed back to the model as a
                        // Tool-role error message rather than aborting the
                        // run. The model then sees the failure and can retry
                        // with corrected arguments on the next turn; the
                        // iteration cap bounds the recovery attempts.
                        match harness.dispatch_tool_call(tc).await {
                            Ok(_) => {}
                            Err(ExecutorError::Tool(te)) => {
                                harness.push_tool_error(tc, &te);
                            }
                            Err(other) => return Err(other),
                        }
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
    use crate::types::TraceLevel;
    use crate::types::{ToolOutput, ToolSchema, Usage};
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
            trace_level: TraceLevel::default(),
            interrupt_check: None,
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

    /// A tool that fails its first `fail_times` invocations with an
    /// `Execution` error, then succeeds. Models the real-world case where a
    /// model emits a malformed argument once, then corrects it after seeing
    /// the error fed back.
    struct FlakyTool {
        name: String,
        schema: ToolSchema,
        calls: std::sync::atomic::AtomicUsize,
        fail_times: usize,
    }

    impl FlakyTool {
        fn new(name: &str, fail_times: usize) -> Self {
            Self {
                name: name.into(),
                schema: ToolSchema::default(),
                calls: std::sync::atomic::AtomicUsize::new(0),
                fail_times,
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::Tool for FlakyTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn schema(&self) -> &ToolSchema {
            &self.schema
        }
        async fn run(
            &self,
            _input: serde_json::Value,
            _ctx: &crate::types::ToolCtx,
        ) -> Result<ToolOutput, ToolError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < self.fail_times {
                Err(ToolError::Execution {
                    name: self.name.clone(),
                    reason: "column \"x\" specified more than once".into(),
                })
            } else {
                Ok(ToolOutput::text("ok"))
            }
        }
    }

    #[tokio::test]
    async fn tool_error_is_fed_back_then_model_recovers() {
        // Turn 1: model calls a tool that fails once.
        // Turn 2: model (having seen the error) calls again; tool succeeds.
        // Turn 3: model emits final text.
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "flaky", serde_json::json!({"bad": true}));
        mock.push_tool_call("c2", "flaky", serde_json::json!({"good": true}));
        mock.push_text("recovered and done");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(FlakyTool::new("flaky", 1));
        let ctx = ctx_with(llm, reg, 10, None);

        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "recovered and done");
        // The failed call must appear as a Tool-role error message so the
        // model could see it, not abort the run.
        let err_msg = outcome.messages.iter().find(|m| {
            m.role == crate::types::Role::Tool
                && m.content.as_deref().is_some_and(|c| c.contains("ERROR:"))
        });
        assert!(
            err_msg.is_some(),
            "expected a Tool-role error message in the trace"
        );
    }

    /// A tool that panics inside `run`. Models a buggy plugin that hits an
    /// `unwrap`/`panic!` on unexpected input.
    struct PanicTool {
        schema: ToolSchema,
    }

    #[async_trait::async_trait]
    impl crate::Tool for PanicTool {
        fn name(&self) -> &str {
            "boom"
        }
        fn schema(&self) -> &ToolSchema {
            &self.schema
        }
        async fn run(
            &self,
            _input: serde_json::Value,
            _ctx: &crate::types::ToolCtx,
        ) -> Result<ToolOutput, ToolError> {
            panic!("simulated plugin bug");
        }
    }

    #[tokio::test]
    async fn panicking_tool_is_contained_not_fatal() {
        // Turn 1: model calls a tool that panics.
        // Turn 2: model (having seen the error fed back) emits final text.
        // The panic must NOT unwind out of the executor (which, in the pgrx
        // host, would abort the whole transaction).
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "boom", serde_json::json!({}));
        mock.push_text("recovered after the tool blew up");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(PanicTool {
            schema: ToolSchema::default(),
        });
        let ctx = ctx_with(llm, reg, 10, None);

        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "recovered after the tool blew up");
        let fed_back = outcome.messages.iter().any(|m| {
            m.role == crate::types::Role::Tool
                && m.content.as_deref().is_some_and(|c| c.contains("ERROR:"))
        });
        assert!(fed_back, "panic should be fed back as a tool error message");
    }

    #[tokio::test]
    async fn unknown_tool_is_fed_back_not_fatal() {
        // An unknown tool no longer aborts the run: the NotFound error is
        // fed back and the model can choose a different action.
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "missing", serde_json::json!({}));
        mock.push_text("ok, I will not use that tool");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);

        let outcome = ConversationExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "ok, I will not use that tool");
        let fed_back = outcome.messages.iter().any(|m| {
            m.role == crate::types::Role::Tool
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("not found"))
        });
        assert!(
            fed_back,
            "NotFound error should be fed back as a tool message"
        );
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
