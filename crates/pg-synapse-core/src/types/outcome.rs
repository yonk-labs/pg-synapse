//! Executor outcome: the value returned by [`crate::Executor::execute`].

use serde::{Deserialize, Serialize};

use super::llm::ToolCall;
use super::message::Message;

/// Result envelope from one successful executor run.
///
/// Errors that abort the run before completion surface as
/// [`crate::ExecutorError`] instead; this struct is what the executor returns
/// when control flow ends normally (including soft terminations like
/// `MaxIterations`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExecutorOutcome {
    /// The assistant's final user-facing text. Empty when the run produced no text.
    pub output: String,
    /// Full message history captured during the run.
    pub messages: Vec<Message>,
    /// Every tool call the model made, in order.
    pub tool_calls: Vec<ToolCall>,
    /// Total prompt tokens across all LLM calls in the run.
    pub tokens_in: u32,
    /// Total completion tokens across all LLM calls in the run.
    pub tokens_out: u32,
    /// Total cost in USD; `None` when pricing data is not registered.
    pub cost_usd: Option<f64>,
    /// Wall-clock duration of the run in milliseconds.
    pub duration_ms: u64,
    /// Why the run terminated.
    pub status: OutcomeStatus,
}

/// Reason a run ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// The agent emitted a final answer.
    #[default]
    Completed,
    /// Hit the iteration cap without finalizing.
    MaxIterations,
    /// Wall-clock budget exhausted.
    TimedOut,
    /// Cost cap tripped mid-run.
    CostCapExceeded,
    /// A recoverable error was surfaced; detail string captures the reason.
    Errored(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_default_completed() {
        let o = ExecutorOutcome::default();
        assert_eq!(o.status, OutcomeStatus::Completed);
        assert!(o.output.is_empty());
    }

    #[test]
    fn outcome_roundtrips() {
        let o = ExecutorOutcome {
            output: "ok".into(),
            messages: vec![],
            tool_calls: vec![],
            tokens_in: 10,
            tokens_out: 20,
            cost_usd: Some(0.01),
            duration_ms: 42,
            status: OutcomeStatus::Completed,
        };
        let s = serde_json::to_string(&o).unwrap();
        let back: ExecutorOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(o, back);
    }

    #[test]
    fn outcome_status_errored_carries_detail() {
        let s = OutcomeStatus::Errored("nope".into());
        let json = serde_json::to_string(&s).unwrap();
        let back: OutcomeStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn outcome_debug_includes_status() {
        let o = ExecutorOutcome {
            status: OutcomeStatus::TimedOut,
            ..Default::default()
        };
        let s = format!("{:?}", o);
        assert!(s.contains("TimedOut"));
    }
}
