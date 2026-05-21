//! [`ReActExecutor`]: same loop as `ConversationExecutor` with a system-prompt
//! addendum that encourages the "Thought / Action / Observation" pattern.
//!
//! Spec reference: design.md Section 5 and M2 plan Task 2.3.
//!
//! Modern LLMs prefer provider-native function calling (the `tool_calls` field
//! on `CompletionResponse`) over text-tagged actions, so this executor trusts
//! the LLM to issue tool calls natively. The addendum nudges chain-of-thought
//! style reasoning without forcing a parser; behavior is otherwise identical
//! to `ConversationExecutor`.

use async_trait::async_trait;

use crate::Executor;
use crate::error::ExecutorError;
use crate::executor::loop_harness::{LoopHarness, TurnResult};
use crate::types::{ExecutionContext, ExecutorOutcome, OutcomeStatus};

/// System-prompt addendum prepended (newline-joined) to `ctx.system_prompt`.
pub const REACT_SYSTEM_ADDENDUM: &str = "When solving problems, think step by step. \
For each action, first reason about what to do, then call the appropriate tool. \
After observing the tool result, reason again before the next step.";

/// ReAct-style executor: same loop as conversation, with a reasoning addendum.
#[derive(Default, Debug)]
pub struct ReActExecutor;

#[async_trait]
impl Executor for ReActExecutor {
    async fn execute(&self, ctx: ExecutionContext) -> Result<ExecutorOutcome, ExecutorError> {
        let mut harness = LoopHarness::new_with_prefix(&ctx, REACT_SYSTEM_ADDENDUM);
        harness.seed_messages();

        loop {
            harness.bump_iteration();
            if let Err(e) = harness.check_iteration_cap() {
                return match e {
                    ExecutorError::MaxIterationsReached(_) => {
                        Ok(harness.finalize(String::new(), OutcomeStatus::MaxIterations))
                    }
                    other => Err(other),
                };
            }
            harness.check_cost_cap()?;

            match harness.one_llm_turn().await {
                Ok(TurnResult::AssistantText(text)) => {
                    return Ok(harness.finalize(text, OutcomeStatus::Completed));
                }
                Ok(TurnResult::ToolCalls(calls)) => {
                    for tc in &calls {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LlmError;
    use crate::llm::LlmProvider;
    use crate::testing::{MockLlmProvider, MockTool};
    use crate::tool::ToolRegistry;
    use crate::types::TraceLevel;
    use crate::types::{Role, ToolOutput, Usage};
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
            executor_name: "react".into(),
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
        }
    }

    #[tokio::test]
    async fn happy_path_returns_text() {
        let mock = MockLlmProvider::new("m");
        mock.push_text("the answer is 42");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);

        let outcome = ReActExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "the answer is 42");
    }

    #[tokio::test]
    async fn system_prompt_includes_react_addendum() {
        let mock = MockLlmProvider::new("m");
        mock.push_text("done");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);

        let outcome = ReActExecutor.execute(ctx).await.unwrap();
        let system_msg = outcome
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .expect("system message present");
        let body = system_msg.content.as_deref().unwrap_or("");
        assert!(
            body.contains("step by step"),
            "expected ReAct addendum in system prompt; got: {body}"
        );
        assert!(body.contains("be helpful"));
    }

    #[tokio::test]
    async fn tool_call_then_text() {
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "echo", serde_json::json!({"q": "x"}));
        mock.push_text("found it");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("here")));
        let ctx = ctx_with(llm, reg, 10, None);

        let outcome = ReActExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "found it");
        assert_eq!(outcome.tool_calls.len(), 1);
    }

    #[tokio::test]
    async fn max_iterations_returns_soft_outcome() {
        let mock = MockLlmProvider::new("m");
        for i in 0..20 {
            mock.push_tool_call(format!("c{i}"), "echo", serde_json::json!({"i": i}));
        }
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("ok")));
        let ctx = ctx_with(llm, reg, 2, None);

        let outcome = ReActExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::MaxIterations);
    }

    #[tokio::test]
    async fn cost_cap_trips_mid_loop() {
        let mock = MockLlmProvider::new("m");
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
        mock.push_text("never");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("ok")));
        let ctx = ctx_with(llm, reg, 5, Some(0.10));

        let err = ReActExecutor.execute(ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::CostCapExceeded { .. }));
    }

    #[tokio::test]
    async fn unknown_tool_is_fed_back_not_fatal() {
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "ghost", serde_json::json!({}));
        mock.push_text("recovered");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);

        let outcome = ReActExecutor.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        let fed_back = outcome.messages.iter().any(|m| {
            m.role == Role::Tool
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
    async fn llm_error_surfaces() {
        let mock = MockLlmProvider::new("m");
        mock.push_error(LlmError::Auth("bad".into()));
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let err = ReActExecutor.execute(ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Llm(LlmError::Auth(_))));
    }
}
