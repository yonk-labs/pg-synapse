//! [`ReflectionExecutor`]: generate, critique, revise.
//!
//! Spec reference: design.md Section 5 ("Reflection executor") and M2 plan
//! Task 2.4. Uses `ctx.judge_llm` for the critique phase when present; falls
//! back to `ctx.llm` otherwise.
//!
//! Reflection in v0.1 is a pure reasoning loop: it does not dispatch tools.
//! If the underlying model returns tool calls during a phase, the executor
//! treats that as the model declining to answer in text and proceeds to the
//! next phase with the issued tool-call IDs recorded in the message log for
//! trace fidelity. This keeps the reflection control-flow simple and matches
//! the typical use case (critique-style reasoning over a primary draft).

use std::sync::Arc;

use async_trait::async_trait;

use crate::Executor;
use crate::error::ExecutorError;
use crate::executor::loop_harness::{LoopHarness, TurnResult};
use crate::llm::LlmProvider;
use crate::types::{ExecutionContext, ExecutorOutcome, OutcomeStatus};

/// Token a critique starts with (or contains) to signal acceptance.
pub const ACCEPT_TOKEN: &str = "[ACCEPT]";

/// Three-phase reasoning executor: generate, critique, revise.
///
/// Construct via [`ReflectionExecutor::default`] (3 revision rounds) or set
/// `max_revisions` explicitly.
#[derive(Debug, Clone, Copy)]
pub struct ReflectionExecutor {
    /// Maximum number of critique-revise rounds after the initial generate.
    pub max_revisions: u32,
}

impl Default for ReflectionExecutor {
    fn default() -> Self {
        Self { max_revisions: 3 }
    }
}

impl ReflectionExecutor {
    /// Construct with an explicit cap on critique-revise rounds.
    pub fn with_max_revisions(max_revisions: u32) -> Self {
        Self { max_revisions }
    }
}

#[async_trait]
impl Executor for ReflectionExecutor {
    async fn execute(&self, ctx: ExecutionContext) -> Result<ExecutorOutcome, ExecutorError> {
        let mut harness = LoopHarness::new(&ctx);
        harness.seed_messages();

        // Phase 1: generate the initial draft via the main provider.
        harness.bump_iteration();
        harness.check_cost_cap()?;
        let mut current = match harness.one_llm_turn().await? {
            TurnResult::AssistantText(t) => t,
            TurnResult::ToolCalls(_) => String::new(),
        };

        // Resolve judge once. Falls back to main when not configured.
        let judge: Arc<dyn LlmProvider> = ctx.judge_llm.clone().unwrap_or_else(|| ctx.llm.clone());

        for _round in 0..self.max_revisions {
            harness.check_cost_cap()?;
            harness.bump_iteration();
            if harness.check_iteration_cap().is_err() {
                return Ok(harness.finalize(current, OutcomeStatus::MaxIterations));
            }

            // Phase 2: critique using the judge provider.
            harness.push_user_message(format!(
                "Critique the previous answer for accuracy and completeness. \
                 If it is acceptable as-is, reply with the literal token {ACCEPT_TOKEN} \
                 followed by a brief confirmation. Otherwise, list concrete \
                 problems and what to change."
            ));
            let critique = match harness.one_llm_turn_with(judge.clone()).await? {
                TurnResult::AssistantText(t) => t,
                TurnResult::ToolCalls(_) => String::new(),
            };

            if critique.contains(ACCEPT_TOKEN) {
                return Ok(harness.finalize(current, OutcomeStatus::Completed));
            }

            harness.check_cost_cap()?;
            harness.bump_iteration();
            if harness.check_iteration_cap().is_err() {
                return Ok(harness.finalize(current, OutcomeStatus::MaxIterations));
            }

            // Phase 3: revise using the main provider.
            harness.push_user_message(
                "Revise your previous answer to address the critique. \
                 Reply with only the revised answer; no preamble.",
            );
            current = match harness.one_llm_turn().await? {
                TurnResult::AssistantText(t) => t,
                TurnResult::ToolCalls(_) => current,
            };
        }

        Ok(harness.finalize(current, OutcomeStatus::Completed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockLlmProvider;
    use crate::tool::ToolRegistry;
    use crate::types::{TraceLevel, Usage};
    use std::time::Duration;
    use uuid::Uuid;

    fn ctx_with(
        llm: Arc<dyn LlmProvider>,
        judge: Option<Arc<dyn LlmProvider>>,
        max_iterations: u32,
        cost_cap_usd: Option<f64>,
    ) -> ExecutionContext {
        ExecutionContext {
            execution_id: Uuid::nil(),
            agent_name: "agent".into(),
            system_prompt: "be precise".into(),
            soul: None,
            input: "what is 2 + 2?".into(),
            executor_name: "reflection".into(),
            tools: Arc::new(ToolRegistry::new()),
            llm,
            judge_llm: judge,
            small_llm: None,
            embeddings: None,
            memory: None,
            compressor: None,
            max_iterations,
            timeout: Duration::from_millis(2000),
            cost_cap_usd,
            caller_role: None,
            trace_level: TraceLevel::default(),
            interrupt_check: None,
        }
    }

    #[tokio::test]
    async fn judge_accepts_first_round() {
        // Generate, critique = ACCEPT.
        let main = MockLlmProvider::new("main");
        main.push_text("4");
        let judge = MockLlmProvider::new("judge");
        judge.push_text("[ACCEPT] looks good");
        let main_arc: Arc<dyn LlmProvider> = Arc::new(main);
        let judge_arc: Arc<dyn LlmProvider> = Arc::new(judge);
        let ctx = ctx_with(main_arc, Some(judge_arc), 20, None);

        let outcome = ReflectionExecutor::default().execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "4");
    }

    #[tokio::test]
    async fn three_revisions_then_stop() {
        // generate, critique, revise, critique, revise, critique, revise => 7 turns
        // (1 generate + 3 critiques + 3 revises). All critiques reject; final answer
        // is the third revision.
        let main = MockLlmProvider::new("main");
        main.push_text("draft 1");
        main.push_text("revision 1");
        main.push_text("revision 2");
        main.push_text("revision 3");

        let judge = MockLlmProvider::new("judge");
        judge.push_text("needs improvement");
        judge.push_text("still wrong");
        judge.push_text("almost there");

        let ctx = ctx_with(
            Arc::new(main),
            Some(Arc::new(judge) as Arc<dyn LlmProvider>),
            20,
            None,
        );
        let outcome = ReflectionExecutor::default().execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "revision 3");
    }

    #[tokio::test]
    async fn falls_back_to_main_llm_when_no_judge() {
        // Without a judge, the main LLM does both generate and critique. We
        // script the main mock to accept on the first critique.
        let main = MockLlmProvider::new("main");
        main.push_text("answer"); // generate
        main.push_text("[ACCEPT] fine"); // critique (also main since no judge)
        let ctx = ctx_with(Arc::new(main), None, 20, None);

        let outcome = ReflectionExecutor::default().execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "answer");
    }

    #[tokio::test]
    async fn critique_uses_judge_when_present() {
        // Verify the judge's model name appears in the recorded request flow:
        // we can't directly read which provider was used, but we can verify
        // the judge's queue was drained (i.e. it was actually called).
        let main = MockLlmProvider::new("main-mdl");
        main.push_text("answer");
        let judge_inner = MockLlmProvider::new("judge-mdl");
        judge_inner.push_text("[ACCEPT] good");
        let judge_arc: Arc<MockLlmProvider> = Arc::new(judge_inner);
        // Wrap as trait object for context; keep concrete for `.queued()`.
        let judge_dyn: Arc<dyn LlmProvider> = judge_arc.clone();
        let ctx = ctx_with(Arc::new(main), Some(judge_dyn), 20, None);

        let outcome = ReflectionExecutor::default().execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(
            judge_arc.queued(),
            0,
            "judge queue should be drained when judge is wired up"
        );
    }

    #[tokio::test]
    async fn cost_cap_trips_across_revisions() {
        let main = MockLlmProvider::new("main");
        main.push_text_with_usage(
            "draft",
            Usage {
                tokens_in: 1,
                tokens_out: 1,
                cost_usd: Some(0.06),
            },
        );
        // critique
        let judge = MockLlmProvider::new("judge");
        judge.push_text_with_usage(
            "needs work",
            Usage {
                tokens_in: 1,
                tokens_out: 1,
                cost_usd: Some(0.06),
            },
        );
        let ctx = ctx_with(
            Arc::new(main),
            Some(Arc::new(judge) as Arc<dyn LlmProvider>),
            20,
            Some(0.10),
        );
        let err = ReflectionExecutor::default()
            .execute(ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ExecutorError::CostCapExceeded { .. }));
    }

    #[tokio::test]
    async fn max_revisions_caps_loop() {
        let main = MockLlmProvider::new("main");
        // generate + (up to 5 revisions)
        for _ in 0..10 {
            main.push_text("draft");
        }
        let judge = MockLlmProvider::new("judge");
        for _ in 0..10 {
            judge.push_text("nope");
        }
        let exec = ReflectionExecutor::with_max_revisions(2);
        let ctx = ctx_with(
            Arc::new(main),
            Some(Arc::new(judge) as Arc<dyn LlmProvider>),
            20,
            None,
        );
        let outcome = exec.execute(ctx).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        // After 2 revision rounds we exit with the last revised draft.
        assert_eq!(outcome.output, "draft");
    }
}
