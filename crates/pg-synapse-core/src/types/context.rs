//! [`ExecutionContext`]: the immutable bundle of inputs and provider handles
//! passed to [`crate::Executor::execute`].

use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::compression::Compressor;
use crate::embedding::EmbeddingProvider;
use crate::llm::LlmProvider;
use crate::memory::MemoryProvider;
use crate::tool::ToolRegistry;

/// Everything an executor needs for one run.
///
/// Holds `Arc`s to trait objects, so it is not `Clone` by `derive`; use
/// [`ExecutionContext::shallow_clone`] to make a shallow copy that shares the
/// same providers and tool registry. Because it owns trait objects, this
/// struct is **not** `Serialize` or `Deserialize` either.
///
/// Spec reference: design.md Section 6 ("Kernel Public Surface").
pub struct ExecutionContext {
    /// UUID for this run; matches `pg_synapse.executions.id`.
    pub execution_id: Uuid,

    /// Agent name (`pg_synapse.agents.name`).
    pub agent_name: String,

    /// Resolved system prompt, including any `soul` prefix.
    pub system_prompt: String,

    /// Optional extended persona block. The host typically prepends this to
    /// `system_prompt` before passing it in; carried separately here for trace
    /// fidelity and so executors can re-emit it.
    pub soul: Option<String>,

    /// End-user input for this run.
    pub input: String,

    /// Which executor implementation this context was routed to.
    pub executor_name: String,

    /// Tools this agent is allowed to call.
    pub tools: Arc<ToolRegistry>,

    /// The `main`-role LLM (always required).
    pub llm: Arc<dyn LlmProvider>,

    /// Optional `judge`-role LLM, used by `ReflectionExecutor` for critique.
    pub judge_llm: Option<Arc<dyn LlmProvider>>,

    /// Optional `small`-role LLM for cheap routing / coercion paths.
    pub small_llm: Option<Arc<dyn LlmProvider>>,

    /// Optional embedding provider for tools or executors that need it.
    pub embeddings: Option<Arc<dyn EmbeddingProvider>>,

    /// Optional memory backend.
    pub memory: Option<Arc<dyn MemoryProvider>>,

    /// Optional compressor.
    pub compressor: Option<Arc<dyn Compressor>>,

    /// Iteration cap (executor turns).
    pub max_iterations: u32,

    /// Wall-clock timeout for the entire run.
    pub timeout: Duration,

    /// Optional USD cost cap.
    pub cost_cap_usd: Option<f64>,

    /// Postgres role that invoked the agent (for `executions.caller_role`).
    pub caller_role: Option<String>,
}

impl ExecutionContext {
    /// Shallow clone: every `Arc` is cloned, but the underlying providers and
    /// tool registry are shared with the original context.
    ///
    /// Named `shallow_clone` rather than `clone` to (a) avoid colliding with
    /// `std::clone::Clone`, which would require every field to be `Clone` (the
    /// `Arc<dyn ...>` fields are not, since trait objects are not `Clone`),
    /// and (b) signal at the call site that providers are shared, not copied.
    pub fn shallow_clone(&self) -> Self {
        Self {
            execution_id: self.execution_id,
            agent_name: self.agent_name.clone(),
            system_prompt: self.system_prompt.clone(),
            soul: self.soul.clone(),
            input: self.input.clone(),
            executor_name: self.executor_name.clone(),
            tools: Arc::clone(&self.tools),
            llm: Arc::clone(&self.llm),
            judge_llm: self.judge_llm.as_ref().map(Arc::clone),
            small_llm: self.small_llm.as_ref().map(Arc::clone),
            embeddings: self.embeddings.as_ref().map(Arc::clone),
            memory: self.memory.as_ref().map(Arc::clone),
            compressor: self.compressor.as_ref().map(Arc::clone),
            max_iterations: self.max_iterations,
            timeout: self.timeout,
            cost_cap_usd: self.cost_cap_usd,
            caller_role: self.caller_role.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockLlmProvider;
    use crate::tool::ToolRegistry;

    fn sample() -> ExecutionContext {
        ExecutionContext {
            execution_id: Uuid::nil(),
            agent_name: "agent".into(),
            system_prompt: "be helpful".into(),
            soul: Some("you are alice".into()),
            input: "hi".into(),
            executor_name: "conversation".into(),
            tools: Arc::new(ToolRegistry::new()),
            llm: Arc::new(MockLlmProvider::new("mock")),
            judge_llm: None,
            small_llm: None,
            embeddings: None,
            memory: None,
            compressor: None,
            max_iterations: 4,
            timeout: Duration::from_millis(1000),
            cost_cap_usd: Some(0.10),
            caller_role: Some("pg_synapse_user".into()),
        }
    }

    #[test]
    fn shallow_clone_shares_arcs() {
        let a = sample();
        let b = a.shallow_clone();
        assert!(Arc::ptr_eq(&a.tools, &b.tools));
        assert!(Arc::ptr_eq(&a.llm, &b.llm));
        assert_eq!(a.agent_name, b.agent_name);
        assert_eq!(a.execution_id, b.execution_id);
    }

    #[test]
    fn carries_optional_providers() {
        let ctx = sample();
        assert!(ctx.embeddings.is_none());
        assert!(ctx.memory.is_none());
        assert!(ctx.compressor.is_none());
        assert!(ctx.judge_llm.is_none());
    }

    #[test]
    fn ctx_holds_supplied_fields() {
        let ctx = sample();
        assert_eq!(ctx.agent_name, "agent");
        assert_eq!(ctx.input, "hi");
        assert_eq!(ctx.max_iterations, 4);
        assert_eq!(ctx.timeout, Duration::from_millis(1000));
        assert_eq!(ctx.cost_cap_usd, Some(0.10));
    }
}
