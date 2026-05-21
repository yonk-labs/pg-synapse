//! Redacted diagnostics export bundle (PS-4).
//!
//! [`export_diagnostics`] turns an [`ExecutorOutcome`] into a
//! serde-serializable [`DiagnosticsBundle`] for support export, applying
//! exactly the trace-level redaction discipline PS-3 already enforces when
//! writing `synapse.executions` / `synapse.messages` / `synapse.traces`:
//!
//! - run metadata (status, output, tokens, cost, duration) is always present;
//! - `messages` are included only when
//!   [`TraceLevel::should_persist_messages`] is true for the run's success;
//! - `events` are included only when [`TraceLevel::should_persist_events`].
//!
//! There is intentionally no `synapse.execution_export` SQL surface: N2.2
//! freezes `synapse.*` for v0.1.1. This is the kernel capability only; the
//! SQL surface is tracked as a v0.2 issue (see `docs/v0.2-issues.md`).

use serde::{Deserialize, Serialize};

use super::message::Message;
use super::outcome::{ExecutorOutcome, OutcomeStatus};
use super::trace::{ExecutionEvent, TraceLevel};

/// A redaction-applied snapshot of one run, safe to hand to support at the
/// agent's configured trace level. Produced by [`export_diagnostics`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsBundle {
    /// The trace level the bundle was redacted at (so a reader knows which
    /// fields could have been gated out).
    pub trace_level: TraceLevel,
    /// Why the run terminated.
    pub status: OutcomeStatus,
    /// The assistant's final user-facing text.
    pub output: String,
    /// Total prompt tokens across the run.
    pub tokens_in: u32,
    /// Total completion tokens across the run.
    pub tokens_out: u32,
    /// Total cost in USD; `None` when pricing is not registered.
    pub cost_usd: Option<f64>,
    /// Wall-clock duration of the run in milliseconds.
    pub duration_ms: u64,
    /// Message history, gated by [`TraceLevel::should_persist_messages`].
    /// Empty when the level redacts it.
    pub messages: Vec<Message>,
    /// Structured events, gated by [`TraceLevel::should_persist_events`].
    /// Empty when the level redacts it.
    pub events: Vec<ExecutionEvent>,
}

/// Build a [`DiagnosticsBundle`] from an outcome, redacting message and
/// event detail per `trace_level` exactly as PS-3's persistence path does.
///
/// `run_succeeded` is `status == Completed`, matching the PS-3 writer so a
/// bundle and the persisted rows never disagree about what is included.
pub fn export_diagnostics(outcome: &ExecutorOutcome, trace_level: TraceLevel) -> DiagnosticsBundle {
    let run_succeeded = outcome.status == OutcomeStatus::Completed;

    let messages = if trace_level.should_persist_messages(run_succeeded) {
        outcome.messages.clone()
    } else {
        Vec::new()
    };

    let events = if trace_level.should_persist_events() {
        outcome.events.clone()
    } else {
        Vec::new()
    };

    DiagnosticsBundle {
        trace_level,
        status: outcome.status.clone(),
        output: outcome.output.clone(),
        tokens_in: outcome.tokens_in,
        tokens_out: outcome.tokens_out,
        cost_usd: outcome.cost_usd,
        duration_ms: outcome.duration_ms,
        messages,
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{Message, Role};
    use crate::types::trace::{EventKind, ExecutionEvent};
    use uuid::Uuid;

    fn outcome_with(status: OutcomeStatus) -> ExecutorOutcome {
        let m = Message {
            execution_id: Uuid::new_v4(),
            seq: 0,
            role: Role::User,
            content: Some("hello".into()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: chrono::Utc::now(),
        };
        ExecutorOutcome {
            output: "final answer".into(),
            messages: vec![m],
            tool_calls: vec![],
            tokens_in: 11,
            tokens_out: 22,
            cost_usd: Some(0.5),
            duration_ms: 123,
            status,
            events: vec![ExecutionEvent {
                kind: EventKind::LlmRequest,
                payload: serde_json::json!({"k": "v"}),
            }],
        }
    }

    #[test]
    fn metadata_is_always_present_even_at_off() {
        let b = export_diagnostics(&outcome_with(OutcomeStatus::Completed), TraceLevel::Off);
        assert_eq!(b.status, OutcomeStatus::Completed);
        assert_eq!(b.output, "final answer");
        assert_eq!(b.tokens_in, 11);
        assert_eq!(b.tokens_out, 22);
        assert_eq!(b.cost_usd, Some(0.5));
        assert_eq!(b.duration_ms, 123);
        assert_eq!(b.trace_level, TraceLevel::Off);
    }

    #[test]
    fn off_redacts_messages_and_events() {
        let b = export_diagnostics(&outcome_with(OutcomeStatus::Completed), TraceLevel::Off);
        assert!(b.messages.is_empty());
        assert!(b.events.is_empty());
    }

    #[test]
    fn error_level_redacts_messages_on_success() {
        let b = export_diagnostics(&outcome_with(OutcomeStatus::Completed), TraceLevel::Error);
        assert!(
            b.messages.is_empty(),
            "successful run: messages redacted at Error"
        );
    }

    #[test]
    fn error_level_keeps_messages_on_failure() {
        let b = export_diagnostics(
            &outcome_with(OutcomeStatus::Errored("boom".into())),
            TraceLevel::Error,
        );
        assert_eq!(b.messages.len(), 1, "failed run: messages kept at Error");
        assert!(b.events.is_empty(), "events still redacted at Error");
    }

    #[test]
    fn info_keeps_messages_redacts_events() {
        let b = export_diagnostics(&outcome_with(OutcomeStatus::Completed), TraceLevel::Info);
        assert_eq!(b.messages.len(), 1);
        assert!(b.events.is_empty());
    }

    #[test]
    fn debug_keeps_messages_and_events() {
        let b = export_diagnostics(&outcome_with(OutcomeStatus::Completed), TraceLevel::Debug);
        assert_eq!(b.messages.len(), 1);
        assert_eq!(b.events.len(), 1);
    }

    #[test]
    fn bundle_roundtrips_through_serde() {
        let b = export_diagnostics(&outcome_with(OutcomeStatus::Completed), TraceLevel::Full);
        let s = serde_json::to_string(&b).unwrap();
        let back: DiagnosticsBundle = serde_json::from_str(&s).unwrap();
        assert_eq!(b, back);
    }
}
