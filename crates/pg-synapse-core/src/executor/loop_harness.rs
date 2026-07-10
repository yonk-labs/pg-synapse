//! Internal primitive shared by [`crate::executor`] implementations.
//!
//! `LoopHarness` is `pub(crate)` and not part of the kernel's public API. It
//! owns the mutable bookkeeping (iteration count, cost accumulator, message
//! history) that every reference executor needs, plus typed helpers for
//! running one LLM turn and dispatching one tool call.
//!
//! The harness deliberately does **not** enforce a wall-clock timeout. Per
//! spec section G4 ("Wall-clock budgets") the timeout is applied by the
//! `Service::call` wrapper or a `tower::Layer` upstream of the executor; the
//! executor itself only knows about iteration and cost caps.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use futures::future::FutureExt;

use crate::error::{ExecutorError, ToolError};
use crate::llm::LlmProvider;
use crate::tool::Tool;
use crate::types::{
    CompletionRequest, CompletionResponse, EventKind, ExecutionContext, ExecutionEvent,
    ExecutorOutcome, Message, OutcomeStatus, Role, ToolCall, ToolCtx, ToolDefinition, ToolOutput,
};

/// Best-effort extraction of a human-readable message from a caught panic
/// payload (`Box<dyn Any + Send>`), which is a `&str` or `String` in practice.
fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Result of one LLM turn.
pub(crate) enum TurnResult {
    /// Model produced final text (no tool calls).
    AssistantText(String),
    /// Model issued one or more tool calls.
    ToolCalls(Vec<ToolCall>),
}

/// Shared loop bookkeeping for executors. Created once per executor run.
pub(crate) struct LoopHarness<'a> {
    ctx: &'a ExecutionContext,
    iteration: u32,
    cost_so_far: f64,
    tokens_in: u32,
    tokens_out: u32,
    messages: Vec<Message>,
    issued_tool_calls: Vec<ToolCall>,
    events: Vec<ExecutionEvent>,
    seq: u32,
    started_at: Instant,
    prepend_system: Option<String>,
}

impl<'a> LoopHarness<'a> {
    /// Construct a fresh harness against the given execution context.
    pub(crate) fn new(ctx: &'a ExecutionContext) -> Self {
        Self {
            ctx,
            iteration: 0,
            cost_so_far: 0.0,
            tokens_in: 0,
            tokens_out: 0,
            messages: Vec::new(),
            issued_tool_calls: Vec::new(),
            events: Vec::new(),
            seq: 0,
            started_at: Instant::now(),
            prepend_system: None,
        }
    }

    /// Construct a harness with an explicit system-prompt prefix.
    ///
    /// The prefix is joined with `ctx.system_prompt` (newline-separated) and
    /// fed to `seed_messages`. Used by executors that need to inject behavior
    /// addenda (for example, ReAct's "Thought / Action / Observation"
    /// instructions).
    #[allow(dead_code)] // wired up by ReActExecutor in Task 2.3
    pub(crate) fn new_with_prefix(ctx: &'a ExecutionContext, prefix: &str) -> Self {
        let mut h = Self::new(ctx);
        h.prepend_system = Some(prefix.to_string());
        h
    }

    /// Append the system prompt + optional soul + user input to the message log.
    ///
    /// Idempotent only at the call-site level: calling more than once will
    /// append duplicate rows, so callers should invoke exactly once at the
    /// top of `execute`.
    pub(crate) fn seed_messages(&mut self) {
        let mut system_parts: Vec<String> = Vec::new();
        if let Some(prefix) = &self.prepend_system {
            if !prefix.trim().is_empty() {
                system_parts.push(prefix.clone());
            }
        }
        if !self.ctx.system_prompt.trim().is_empty() {
            system_parts.push(self.ctx.system_prompt.clone());
        }
        if let Some(soul) = &self.ctx.soul {
            if !soul.trim().is_empty() {
                system_parts.push(soul.clone());
            }
        }
        if !system_parts.is_empty() {
            let body = system_parts.join("\n");
            self.push_message(Role::System, Some(body), None);
        }

        // user input
        self.push_message(Role::User, Some(self.ctx.input.clone()), None);
    }

    /// Run one LLM completion against the configured `main` provider.
    pub(crate) async fn one_llm_turn(&mut self) -> Result<TurnResult, ExecutorError> {
        let provider = self.ctx.llm.clone();
        self.one_llm_turn_with(provider).await
    }

    /// Run one LLM completion against an explicit provider (used by
    /// reflection when calling the judge profile).
    pub(crate) async fn one_llm_turn_with(
        &mut self,
        provider: Arc<dyn LlmProvider>,
    ) -> Result<TurnResult, ExecutorError> {
        // Between-iteration cancellation: every executor turn passes through
        // here, so a host interrupt (e.g. a Postgres statement cancel) aborts
        // the loop before the next LLM call rather than running to the budget.
        self.check_interrupt()?;
        let req = self.build_request(provider.model_name());
        let mut req_payload = serde_json::json!({
            "iteration": self.iteration,
            "model": provider.model_name(),
            "messages": req.messages.len(),
            "tools": req.tools.len(),
        });
        if self.ctx.trace_level.should_persist_raw_payloads() {
            req_payload["raw_messages"] = serde_json::to_value(&req.messages).unwrap_or_default();
        }
        self.record_event(EventKind::LlmRequest, req_payload);
        let resp = provider.complete(req).await?;
        let mut resp_payload = serde_json::json!({
            "iteration": self.iteration,
            "tokens_in": resp.usage.tokens_in,
            "tokens_out": resp.usage.tokens_out,
            "tool_calls": resp.tool_calls.len(),
            "has_text": resp.content.as_deref().is_some_and(|c| !c.is_empty()),
        });
        if self.ctx.trace_level.should_persist_raw_payloads() {
            resp_payload["raw_content"] = serde_json::to_value(&resp.content).unwrap_or_default();
            resp_payload["raw_tool_calls"] =
                serde_json::to_value(&resp.tool_calls).unwrap_or_default();
        }
        self.record_event(EventKind::LlmResponse, resp_payload);
        self.record_usage(&resp);
        self.record_assistant_response(&resp);
        if !resp.tool_calls.is_empty() {
            for tc in &resp.tool_calls {
                self.issued_tool_calls.push(tc.clone());
            }
            return Ok(TurnResult::ToolCalls(resp.tool_calls));
        }
        let text = resp.content.unwrap_or_default();
        Ok(TurnResult::AssistantText(text))
    }

    /// Dispatch a tool call against the active registry.
    ///
    /// On success, appends a Tool-role [`Message`] containing the tool's
    /// output and returns a clone of it. On `ToolError`, returns the typed
    /// error without appending (the caller decides whether to surface it
    /// directly or feed it back to the model).
    pub(crate) async fn dispatch_tool_call(
        &mut self,
        tc: &ToolCall,
    ) -> Result<Message, ExecutorError> {
        let tool: Arc<dyn Tool> = match self.ctx.tools.get(&tc.name) {
            Some(t) => t,
            None => {
                return Err(ExecutorError::Tool(ToolError::NotFound {
                    name: tc.name.clone(),
                }));
            }
        };

        let tool_ctx = ToolCtx {
            execution_id: self.ctx.execution_id,
            caller_role: self.ctx.caller_role.clone(),
            agent_name: Some(self.ctx.agent_name.clone()),
        };

        let mut start_payload = serde_json::json!({ "tool": tc.name, "call_id": tc.id });
        if self.ctx.trace_level.should_persist_raw_payloads() {
            start_payload["args"] = tc.args.clone();
        }
        self.record_event(EventKind::ToolStart, start_payload);

        // Contain a panicking plugin: a tool that hits an `unwrap`/`panic!`
        // must degrade to a tool-error fed back to the model, not unwind out
        // of the executor (which, in the pgrx host, would abort the whole
        // transaction). SQL tools already convert Postgres `ereport` via
        // `PgTryBuilder` upstream, so any unwind reaching here is a pure-Rust
        // panic that is safe to catch and turn into a typed error.
        let result = match AssertUnwindSafe(tool.run(tc.args.clone(), &tool_ctx))
            .catch_unwind()
            .await
        {
            Ok(r) => r,
            Err(panic) => Err(ToolError::Execution {
                name: tc.name.clone(),
                reason: format!("tool panicked: {}", panic_reason(&panic)),
            }),
        };
        match result {
            Ok(output) => {
                let mut end_payload =
                    serde_json::json!({ "tool": tc.name, "call_id": tc.id, "ok": true });
                if self.ctx.trace_level.should_persist_raw_payloads() {
                    end_payload["output"] = match &output {
                        ToolOutput::Text(s) => serde_json::Value::String(s.clone()),
                        ToolOutput::Json(v) => v.clone(),
                        ToolOutput::Empty => serde_json::Value::Null,
                    };
                }
                self.record_event(EventKind::ToolEnd, end_payload);
                Ok(self.push_tool_result(tc, &output))
            }
            Err(e) => Err(ExecutorError::Tool(e)),
        }
    }

    /// Consult the host interrupt probe (if any). Returns
    /// [`ExecutorError::Cancelled`] when the host signals the run should abort.
    pub(crate) fn check_interrupt(&self) -> Result<(), ExecutorError> {
        if let Some(check) = &self.ctx.interrupt_check {
            if let Some(reason) = check() {
                return Err(ExecutorError::Cancelled(reason));
            }
        }
        Ok(())
    }

    /// Check the configured cost cap. Returns an error if `cost_so_far`
    /// has reached or exceeded the cap.
    pub(crate) fn check_cost_cap(&self) -> Result<(), ExecutorError> {
        if let Some(cap) = self.ctx.cost_cap_usd {
            if self.cost_so_far >= cap {
                return Err(ExecutorError::CostCapExceeded {
                    cap,
                    spent: self.cost_so_far,
                });
            }
        }
        Ok(())
    }

    /// Check the iteration cap. Returns an error if `iteration` has passed
    /// `ctx.max_iterations`.
    pub(crate) fn check_iteration_cap(&mut self) -> Result<(), ExecutorError> {
        if self.iteration > self.ctx.max_iterations {
            self.record_event(
                EventKind::IterationCapCheck,
                serde_json::json!({
                    "iteration": self.iteration,
                    "max": self.ctx.max_iterations,
                    "tripped": true,
                }),
            );
            return Err(ExecutorError::MaxIterationsReached(self.ctx.max_iterations));
        }
        Ok(())
    }

    /// Bump the iteration counter. Call once at the top of each loop turn.
    pub(crate) fn bump_iteration(&mut self) {
        self.iteration += 1;
    }

    /// Finalize the run into an [`ExecutorOutcome`].
    pub(crate) fn finalize(self, output: String, status: OutcomeStatus) -> ExecutorOutcome {
        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        let cost_usd = if self.cost_so_far > 0.0 {
            Some(self.cost_so_far)
        } else {
            None
        };
        ExecutorOutcome {
            output,
            messages: self.messages,
            tool_calls: self.issued_tool_calls,
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            cost_usd,
            duration_ms,
            status,
            events: self.events,
        }
    }

    /// Record a structured trace event. Events are always collected in memory
    /// (cheap; a handful per run) and gated for persistence by `trace_level`
    /// in the host's `log_execution`, mirroring how messages are handled.
    fn record_event(&mut self, kind: EventKind, payload: serde_json::Value) {
        self.events.push(ExecutionEvent { kind, payload });
    }

    /// Borrow the recorded trace events.
    #[allow(dead_code)] // used by tests + the traces writer via the outcome
    pub(crate) fn events(&self) -> &[ExecutionEvent] {
        &self.events
    }

    /// Borrow the recorded message log.
    #[allow(dead_code)] // used by tests + future executors
    pub(crate) fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The current iteration counter value.
    #[allow(dead_code)]
    pub(crate) fn iteration(&self) -> u32 {
        self.iteration
    }

    /// The accumulated USD cost across all LLM turns so far.
    #[allow(dead_code)] // used by tests + future executors
    pub(crate) fn cost_so_far(&self) -> f64 {
        self.cost_so_far
    }

    /// The accumulated input tokens across all LLM turns so far.
    #[allow(dead_code)] // used by tests + future executors
    pub(crate) fn tokens_in(&self) -> u32 {
        self.tokens_in
    }

    /// The accumulated output tokens across all LLM turns so far.
    #[allow(dead_code)] // used by tests + future executors
    pub(crate) fn tokens_out(&self) -> u32 {
        self.tokens_out
    }

    /// Append a user-role message between turns.
    ///
    /// Used by executors that drive multi-phase loops (for example
    /// reflection's critique and revise prompts).
    #[allow(dead_code)] // wired up by ReflectionExecutor in Task 2.4
    pub(crate) fn push_user_message(&mut self, content: impl Into<String>) {
        self.push_message(Role::User, Some(content.into()), None);
    }

    /// Append a system-role message mid-run.
    #[allow(dead_code)] // wired up by ReflectionExecutor in Task 2.4
    pub(crate) fn push_system_message(&mut self, content: impl Into<String>) {
        self.push_message(Role::System, Some(content.into()), None);
    }

    // ----- private helpers -----

    fn build_request(&self, model: &str) -> CompletionRequest {
        let mut tools = Vec::new();
        for name in self.ctx.tools.names() {
            if let Some(tool) = self.ctx.tools.get(&name) {
                tools.push(ToolDefinition {
                    name: name.clone(),
                    description: String::new(),
                    schema: tool.schema().clone(),
                });
            }
        }
        CompletionRequest {
            messages: self.messages.clone(),
            tools,
            model: Some(model.to_string()),
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        }
    }

    fn record_usage(&mut self, resp: &CompletionResponse) {
        self.tokens_in = self.tokens_in.saturating_add(resp.usage.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(resp.usage.tokens_out);
        if let Some(c) = resp.usage.cost_usd {
            self.cost_so_far += c;
        }
    }

    fn record_assistant_response(&mut self, resp: &CompletionResponse) {
        if resp.tool_calls.is_empty() {
            let content = resp.content.clone().unwrap_or_default();
            self.push_message(Role::Assistant, Some(content), None);
            return;
        }
        // One assistant message per tool call: keeps the trace row-shaped
        // for SQL consumers and mirrors how providers serialize them.
        for tc in &resp.tool_calls {
            let mut msg = self.new_message(Role::Assistant);
            msg.tool_call_id = Some(tc.id.clone());
            msg.tool_name = Some(tc.name.clone());
            msg.tool_input = Some(tc.args.clone());
            self.append(msg);
        }
    }

    fn push_tool_result(&mut self, tc: &ToolCall, output: &ToolOutput) -> Message {
        let mut msg = self.new_message(Role::Tool);
        msg.tool_call_id = Some(tc.id.clone());
        msg.tool_name = Some(tc.name.clone());
        msg.tool_input = Some(tc.args.clone());
        let (content, json) = match output {
            ToolOutput::Text(s) => (Some(s.clone()), Some(serde_json::Value::String(s.clone()))),
            ToolOutput::Json(v) => (Some(v.to_string()), Some(v.clone())),
            ToolOutput::Empty => (None, None),
        };
        msg.content = content;
        msg.tool_output = json;
        let snapshot = msg.clone();
        self.append(msg);
        snapshot
    }

    /// Append a Tool-role message representing a *failed* tool call.
    ///
    /// Feeding the error back into the message log (instead of aborting the
    /// run) lets the next LLM turn observe what went wrong and self-correct
    /// the offending argument. The iteration cap still bounds how many
    /// recovery attempts the model gets.
    pub(crate) fn push_tool_error(&mut self, tc: &ToolCall, err: &ToolError) {
        let mut msg = self.new_message(Role::Tool);
        msg.tool_call_id = Some(tc.id.clone());
        msg.tool_name = Some(tc.name.clone());
        msg.tool_input = Some(tc.args.clone());
        let text = format!("ERROR: {err}");
        msg.content = Some(text.clone());
        msg.tool_output = Some(serde_json::json!({ "error": text }));
        self.append(msg);
        self.record_event(
            EventKind::ToolError,
            serde_json::json!({
                "tool": tc.name,
                "call_id": tc.id,
                "error": err.to_string(),
            }),
        );
    }

    fn push_message(
        &mut self,
        role: Role,
        content: Option<String>,
        tool_output: Option<serde_json::Value>,
    ) {
        let mut msg = self.new_message(role);
        msg.content = content;
        msg.tool_output = tool_output;
        self.append(msg);
    }

    fn new_message(&self, role: Role) -> Message {
        Message {
            execution_id: self.ctx.execution_id,
            seq: 0, // overwritten by `append`
            role,
            content: None,
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: Utc::now(),
        }
    }

    fn append(&mut self, mut msg: Message) {
        msg.seq = self.seq;
        self.seq += 1;
        self.messages.push(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MockLlmProvider, MockTool};
    use crate::tool::ToolRegistry;
    use crate::types::TraceLevel;
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
            soul: Some("you are alice".into()),
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
            caller_role: Some("pg_synapse_user".into()),
            trace_level: TraceLevel::default(),
            interrupt_check: None,
        }
    }

    #[test]
    fn seed_messages_appends_system_and_user() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let msgs = h.messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
        let sys = msgs[0].content.as_deref().unwrap();
        assert!(sys.contains("be helpful"));
        assert!(sys.contains("you are alice"));
        assert_eq!(msgs[1].role, Role::User);
        assert_eq!(msgs[1].content.as_deref(), Some("hello"));
        assert_eq!(msgs[0].seq, 0);
        assert_eq!(msgs[1].seq, 1);
    }

    #[test]
    fn seed_messages_uses_prefix_when_present() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new_with_prefix(&ctx, "REACT-ADDENDUM");
        h.seed_messages();
        let sys = h.messages()[0].content.as_deref().unwrap();
        assert!(sys.starts_with("REACT-ADDENDUM"));
    }

    #[tokio::test]
    async fn one_llm_turn_returns_text() {
        let mock = MockLlmProvider::new("m");
        mock.push_text("hi back");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let res = h.one_llm_turn().await.unwrap();
        match res {
            TurnResult::AssistantText(s) => assert_eq!(s, "hi back"),
            _ => panic!("expected text"),
        }
        // assistant message recorded
        assert_eq!(h.messages().last().unwrap().role, Role::Assistant);
    }

    #[tokio::test]
    async fn one_llm_turn_returns_tool_calls() {
        let mock = MockLlmProvider::new("m");
        mock.push_tool_call("c1", "echo", serde_json::json!({"x": 1}));
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("ok")));
        let ctx = ctx_with(llm, reg, 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let res = h.one_llm_turn().await.unwrap();
        match res {
            TurnResult::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "echo");
            }
            _ => panic!("expected tool calls"),
        }
        // assistant tool-call message recorded
        let last = h.messages().last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(last.tool_name.as_deref(), Some("echo"));
    }

    #[tokio::test]
    async fn dispatch_tool_call_success_appends_tool_message() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new(
            "echo",
            ToolOutput::Json(serde_json::json!({"a": 1})),
        ));
        let ctx = ctx_with(llm, reg, 5, None);
        let mut h = LoopHarness::new(&ctx);
        let tc = ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            args: serde_json::json!({}),
        };
        let msg = h.dispatch_tool_call(&tc).await.unwrap();
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(msg.tool_output, Some(serde_json::json!({"a": 1})));
    }

    #[tokio::test]
    async fn dispatch_tool_call_not_found_returns_typed_error() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        let tc = ToolCall {
            id: "c1".into(),
            name: "missing".into(),
            args: serde_json::json!({}),
        };
        let err = h.dispatch_tool_call(&tc).await.unwrap_err();
        match err {
            ExecutorError::Tool(ToolError::NotFound { name }) => assert_eq!(name, "missing"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn check_cost_cap_under_passes() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, Some(0.50));
        let mut h = LoopHarness::new(&ctx);
        h.cost_so_far = 0.10;
        assert!(h.check_cost_cap().is_ok());
    }

    #[test]
    fn check_cost_cap_at_or_above_trips() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, Some(0.10));
        let mut h = LoopHarness::new(&ctx);
        h.cost_so_far = 0.10;
        match h.check_cost_cap().unwrap_err() {
            ExecutorError::CostCapExceeded { cap, spent } => {
                assert_eq!(cap, 0.10);
                assert!(spent >= 0.10);
            }
            other => panic!("unexpected: {other:?}"),
        }
        h.cost_so_far = 0.50;
        assert!(matches!(
            h.check_cost_cap(),
            Err(ExecutorError::CostCapExceeded { .. })
        ));
    }

    #[test]
    fn check_iteration_cap_below_and_above() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 2, None);
        let mut h = LoopHarness::new(&ctx);
        assert!(h.check_iteration_cap().is_ok());
        h.bump_iteration();
        h.bump_iteration();
        assert!(h.check_iteration_cap().is_ok()); // iteration=2, cap=2 still OK
        h.bump_iteration();
        match h.check_iteration_cap().unwrap_err() {
            ExecutorError::MaxIterationsReached(n) => assert_eq!(n, 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn finalize_carries_messages_and_status() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let out = h.finalize("done".into(), OutcomeStatus::Completed);
        assert_eq!(out.output, "done");
        assert_eq!(out.status, OutcomeStatus::Completed);
        assert_eq!(out.messages.len(), 2);
    }

    #[tokio::test]
    async fn one_llm_turn_records_usage() {
        let mock = MockLlmProvider::new("m");
        mock.push_text_with_usage(
            "hi",
            Usage {
                tokens_in: 10,
                tokens_out: 20,
                cost_usd: Some(0.05),
            },
        );
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let _ = h.one_llm_turn().await.unwrap();
        assert_eq!(h.tokens_in(), 10);
        assert_eq!(h.tokens_out(), 20);
        assert!((h.cost_so_far() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn push_user_and_system_messages_use_running_seq() {
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        h.push_system_message("be more concise");
        h.push_user_message("retry");
        let msgs = h.messages();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[2].role, Role::System);
        assert_eq!(msgs[3].role, Role::User);
        assert_eq!(msgs[3].seq, 3);
    }

    #[tokio::test]
    async fn records_execution_events_for_llm_and_tool() {
        use crate::types::EventKind;
        let mock = MockLlmProvider::new("m");
        mock.push_text("hi back");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("echo", ToolOutput::text("ok")));
        let ctx = ctx_with(llm, reg, 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let _ = h.one_llm_turn().await.unwrap();
        let tc = ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            args: serde_json::json!({}),
        };
        let _ = h.dispatch_tool_call(&tc).await.unwrap();

        let kinds: Vec<EventKind> = h.events().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::LlmRequest,
                EventKind::LlmResponse,
                EventKind::ToolStart,
                EventKind::ToolEnd,
            ]
        );
        // Tool events carry the call identity so a SQL consumer can join them.
        let start = h
            .events()
            .iter()
            .find(|e| e.kind == EventKind::ToolStart)
            .unwrap();
        assert_eq!(start.payload["tool"], "echo");
        assert_eq!(start.payload["call_id"], "c1");
    }

    #[tokio::test]
    async fn records_tool_error_event() {
        use crate::types::EventKind;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        let tc = ToolCall {
            id: "c9".into(),
            name: "bad".into(),
            args: serde_json::json!({}),
        };
        h.push_tool_error(&tc, &ToolError::NotFound { name: "bad".into() });
        let ev = h.events().last().unwrap();
        assert_eq!(ev.kind, EventKind::ToolError);
        assert_eq!(ev.payload["tool"], "bad");
        assert!(ev.payload["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn finalize_carries_recorded_events() {
        use crate::types::EventKind;
        let mock = MockLlmProvider::new("m");
        mock.push_text("done");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let ctx = ctx_with(llm, ToolRegistry::new(), 5, None);
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let _ = h.one_llm_turn().await.unwrap();
        let out = h.finalize("done".into(), OutcomeStatus::Completed);
        assert!(
            out.events.iter().any(|e| e.kind == EventKind::LlmResponse),
            "events must survive into the outcome for the traces writer"
        );
    }

    #[tokio::test]
    async fn one_llm_turn_with_uses_provided_provider() {
        let main = MockLlmProvider::new("main-model");
        main.push_text("ignored");
        let judge = MockLlmProvider::new("judge-model");
        judge.push_text("[ACCEPT]");
        let judge_arc: Arc<dyn LlmProvider> = Arc::new(judge);
        let main_arc: Arc<dyn LlmProvider> = Arc::new(main);
        let mut ctx = ctx_with(main_arc, ToolRegistry::new(), 5, None);
        ctx.judge_llm = Some(judge_arc.clone());
        let mut h = LoopHarness::new(&ctx);
        h.seed_messages();
        let res = h.one_llm_turn_with(judge_arc).await.unwrap();
        match res {
            TurnResult::AssistantText(s) => assert_eq!(s, "[ACCEPT]"),
            _ => panic!("expected text"),
        }
    }
}
