//! Default conversation-compaction plugin for pg_synapse.
//!
//! Implements the core [`Compressor`] trait with a deterministic, LLM-free
//! strategy: keep the system prompt and the most-recent messages that fit the
//! token budget, collapse everything older into a short extractive digest.
//!
//! Per spec G8 and ADR D14, compaction is a *plugin*, never a core default.
//! An LLM-summarizing compressor is a separate plugin; this one is the
//! always-available fallback the recovery wrapper (PS-2c) composes onto a
//! provider when a context-length overflow occurs.
//!
//! ## Token estimation
//!
//! The [`Compressor`] trait hands the implementation only `&[Message]` and a
//! [`CompressionBudget`] (no tokenizer, no provider). This plugin uses the
//! industry rule-of-thumb of ~4 characters per token. It is intentionally
//! conservative (rounds up, floors at 1 token per message) so it never
//! *under*-estimates and leaves the window over budget.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use pg_synapse_core::Compressor;
use pg_synapse_core::error::CompressionError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{Compressed, CompressionBudget, Message, Role};
use tracing::debug;

/// Max characters of a single collapsed message rendered into the digest.
const DIGEST_SNIPPET_CHARS: usize = 200;

/// Estimate the token cost of a string at ~4 chars/token, rounded up, with a
/// floor of 1 so an empty-but-present message still costs something.
fn estimate_tokens(s: &str) -> u32 {
    let chars = s.chars().count();
    (chars.div_ceil(4)).max(1) as u32
}

/// Flatten a message into the text a model would actually see, for both
/// estimation and digest rendering.
fn message_text(m: &Message) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = &m.content {
        if !c.is_empty() {
            parts.push(c.clone());
        }
    }
    if let Some(name) = &m.tool_name {
        parts.push(format!("tool={name}"));
    }
    if let Some(input) = &m.tool_input {
        parts.push(format!("input={input}"));
    }
    if let Some(output) = &m.tool_output {
        parts.push(format!("output={output}"));
    }
    parts.join(" ")
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}...")
}

/// Deterministic, LLM-free conversation compressor.
///
/// Strategy: always retain the leading system message (if any) plus the
/// newest messages that fit `budget.target_tokens`; collapse the rest into a
/// single extractive summary. Fails with [`CompressionError::BudgetTooSmall`]
/// when even the minimum retained set exceeds `budget.hard_limit_tokens`.
#[derive(Debug, Default, Clone)]
pub struct DefaultCompressor;

impl DefaultCompressor {
    /// Construct the compressor. No configuration required.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Compressor for DefaultCompressor {
    async fn compress(
        &self,
        messages: &[Message],
        budget: CompressionBudget,
    ) -> Result<Compressed, CompressionError> {
        if messages.is_empty() {
            return Ok(Compressed::default());
        }

        // Index 0 is kept verbatim when it is the system prompt: an agent
        // without its system prompt is a different agent.
        let keep_system = matches!(messages.first(), Some(m) if m.role == Role::System);
        let system_idx: Option<usize> = if keep_system { Some(0) } else { None };

        let per_msg: Vec<u32> = messages
            .iter()
            .map(|m| estimate_tokens(&message_text(m)))
            .collect();
        let system_cost = system_idx.map(|i| per_msg[i]).unwrap_or(0);

        // Walk newest -> oldest, keeping recent messages until the running
        // total (system + recent) would exceed the soft target.
        let target = budget.target_tokens;
        let mut kept_recent: Vec<usize> = Vec::new();
        let mut running = system_cost;
        for i in (0..messages.len()).rev() {
            if Some(i) == system_idx {
                continue;
            }
            let cost = per_msg[i];
            if running.saturating_add(cost) > target && !kept_recent.is_empty() {
                break;
            }
            running = running.saturating_add(cost);
            kept_recent.push(i);
            if running >= target {
                break;
            }
        }
        kept_recent.sort_unstable();

        // Anything neither the kept system message nor a kept-recent message
        // is collapsed into the digest.
        let kept: std::collections::HashSet<usize> =
            kept_recent.iter().copied().chain(system_idx).collect();
        let collapsed_indices: Vec<u32> = (0..messages.len())
            .filter(|i| !kept.contains(i))
            .map(|i| i as u32)
            .collect();

        if collapsed_indices.is_empty() {
            // Everything already fits the target: nothing to do.
            return Ok(Compressed::default());
        }

        let mut lines: Vec<String> = vec![format!(
            "[compacted {} earlier message(s)]",
            collapsed_indices.len()
        )];
        for &i in &collapsed_indices {
            let m = &messages[i as usize];
            let snippet = truncate_chars(&message_text(m), DIGEST_SNIPPET_CHARS);
            lines.push(format!("- {}: {}", role_str(m.role), snippet));
        }
        let summary = lines.join("\n");

        // The retained window after compaction = system + kept-recent + the
        // digest itself. If that still blows the hard limit, the budget is
        // genuinely too small for this conversation.
        let retained_tokens = system_cost
            .saturating_add(kept_recent.iter().map(|&i| per_msg[i]).sum())
            .saturating_add(estimate_tokens(&summary));
        if retained_tokens > budget.hard_limit_tokens {
            return Err(CompressionError::BudgetTooSmall {
                needed: retained_tokens,
                available: budget.hard_limit_tokens,
            });
        }

        debug!(
            collapsed = collapsed_indices.len(),
            retained_tokens, "default compaction pass"
        );
        Ok(Compressed {
            summary,
            collapsed_indices,
        })
    }
}

/// Plugin that installs [`DefaultCompressor`] as the host's compressor.
pub struct CompactionPlugin;

impl CompactionPlugin {
    /// Create the plugin. No configuration required.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CompactionPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CompactionPlugin {
    fn name(&self) -> &str {
        "pg-synapse-compaction"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry.compressor = Some(std::sync::Arc::new(DefaultCompressor::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            execution_id: Uuid::nil(),
            seq: 0,
            role,
            content: Some(content.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn empty_messages_returns_empty_compressed() {
        let c = DefaultCompressor::new();
        let out = c
            .compress(
                &[],
                CompressionBudget {
                    target_tokens: 100,
                    hard_limit_tokens: 200,
                },
            )
            .await
            .unwrap();
        assert_eq!(out, Compressed::default());
    }

    #[tokio::test]
    async fn no_op_when_already_within_target() {
        let c = DefaultCompressor::new();
        let msgs = vec![msg(Role::User, "hi"), msg(Role::Assistant, "hello")];
        let out = c
            .compress(
                &msgs,
                CompressionBudget {
                    target_tokens: 1000,
                    hard_limit_tokens: 2000,
                },
            )
            .await
            .unwrap();
        assert!(out.collapsed_indices.is_empty());
        assert!(out.summary.is_empty());
    }

    #[tokio::test]
    async fn collapses_old_keeps_recent_within_target() {
        let c = DefaultCompressor::new();
        let big = "x".repeat(400); // ~100 tokens each
        let mut msgs = Vec::new();
        for _ in 0..6 {
            msgs.push(msg(Role::User, &big));
        }
        // Distinct newest message so we can assert it survived verbatim.
        msgs.push(msg(Role::Assistant, "FINAL-ANSWER"));
        let last = (msgs.len() - 1) as u32;

        let out = c
            .compress(
                &msgs,
                CompressionBudget {
                    target_tokens: 120,
                    hard_limit_tokens: 5000,
                },
            )
            .await
            .unwrap();

        assert!(
            !out.collapsed_indices.is_empty(),
            "old messages must collapse"
        );
        assert!(
            !out.collapsed_indices.contains(&last),
            "the newest message must be retained, not collapsed"
        );
        assert!(out.summary.contains("compacted"));
    }

    #[tokio::test]
    async fn preserves_system_message_even_when_old() {
        let c = DefaultCompressor::new();
        let big = "y".repeat(400);
        let mut msgs = vec![msg(Role::System, "YOU-ARE-ALICE")];
        for _ in 0..6 {
            msgs.push(msg(Role::User, &big));
        }
        let out = c
            .compress(
                &msgs,
                CompressionBudget {
                    target_tokens: 120,
                    hard_limit_tokens: 5000,
                },
            )
            .await
            .unwrap();
        assert!(
            !out.collapsed_indices.contains(&0),
            "system message at index 0 must never be collapsed"
        );
    }

    #[tokio::test]
    async fn budget_too_small_when_hard_limit_unsatisfiable() {
        let c = DefaultCompressor::new();
        let big = "z".repeat(4000); // ~1000 tokens
        let msgs = vec![
            msg(Role::System, &big),
            msg(Role::User, &big),
            msg(Role::Assistant, &big),
        ];
        let err = c
            .compress(
                &msgs,
                CompressionBudget {
                    target_tokens: 10,
                    hard_limit_tokens: 5,
                },
            )
            .await
            .unwrap_err();
        match err {
            CompressionError::BudgetTooSmall { needed, available } => {
                assert!(needed > available);
                assert_eq!(available, 5);
            }
            other => panic!("expected BudgetTooSmall, got {other:?}"),
        }
    }

    #[test]
    fn plugin_registers_compressor() {
        let mut reg = Registry::new();
        assert!(reg.compressor.is_none());
        CompactionPlugin::new().register(&mut reg);
        assert!(reg.compressor.is_some());
    }
}
