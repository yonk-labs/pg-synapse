//! Conversation messages exchanged between executor, LLM, and tools.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row in `pg_synapse.messages`. Captures any of: a system / user /
/// assistant text turn, an assistant tool-call turn, or the tool's response.
///
/// `tool_*` fields are populated together when `role == Tool`, or on the
/// assistant turn that issued a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Execution this message belongs to.
    pub execution_id: Uuid,
    /// Monotonically increasing sequence number within the execution.
    pub seq: u32,
    /// Whose turn this is.
    pub role: Role,
    /// Text content, if any. `None` for assistant turns that only emit tool calls.
    pub content: Option<String>,
    /// Provider-issued tool-call ID this row refers to.
    pub tool_call_id: Option<String>,
    /// Name of the tool invoked, when relevant.
    pub tool_name: Option<String>,
    /// Arguments the assistant passed to the tool.
    pub tool_input: Option<serde_json::Value>,
    /// Output the tool returned.
    pub tool_output: Option<serde_json::Value>,
    /// Wall-clock timestamp at which this message was recorded.
    pub timestamp: DateTime<Utc>,
}

/// Role of a [`Message`] within a conversation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System prompt (and any `soul` prefix).
    System,
    /// End-user input.
    User,
    /// Assistant (LLM) output.
    Assistant,
    /// Tool response.
    Tool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Message {
        Message {
            execution_id: Uuid::nil(),
            seq: 1,
            role: Role::User,
            content: Some("hi".into()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: DateTime::from_timestamp(0, 0).unwrap(),
        }
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn message_roundtrips_json() {
        let m = sample();
        let s = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn message_equality() {
        let a = sample();
        let b = sample();
        assert_eq!(a, b);
        let mut c = sample();
        c.seq = 2;
        assert_ne!(a, c);
    }

    #[test]
    fn message_debug_format_contains_role() {
        let m = sample();
        let s = format!("{:?}", m);
        assert!(s.contains("User"));
    }
}
