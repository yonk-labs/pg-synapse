//! [`Harness`]: the cost-tiered components around an agent's system prompt.
//!
//! Borrowed from the Harness2 component model (Xu et al. 2026). The
//! kernel treats these as data: always-on text is composed into the system
//! message once per run, skills are exposed through the kernel-provided
//! `load_skill` tool and loaded only when invoked, and tool hints are
//! appended to a failed tool call's message when its error text matches.
//!
//! Hosts load the fields from `synapse.agents`, `synapse.skills`,
//! `synapse.tool_hints` and `synapse.lessons`. The kernel never reads those
//! tables and never persists anything here.

use serde::{Deserialize, Serialize};

/// Upper bound on always-on text (system prompt + soul + guardrails +
/// memory + skill trigger lines), in characters. Every always-on character
/// competes with the model's own output budget on every turn; Harness2
/// found 24k a workable ceiling for small models. Enforced at write time by
/// the SQL surface, re-checked here so a host that bypasses SQL still sees
/// the same rule.
pub const ALWAYS_ON_MAX_CHARS: usize = 24_000;

/// Header the kernel writes above the skill trigger lines.
pub const SKILL_TRIGGER_HEADER: &str =
    "Skills (load one with the `load_skill` tool when its situation applies):";

/// An on-demand procedure. Only `description` is always-on (as one trigger
/// line); `body` enters context when the model calls `load_skill`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Skill {
    /// Identifier the model passes to `load_skill`.
    pub name: String,
    /// One line naming the situation this skill handles.
    pub description: String,
    /// The procedure itself. Not in context until loaded.
    pub body: String,
}

/// A targeted hint appended to a tool error whose text contains
/// `error_pattern`. `tool_name = None` matches any tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolHint {
    /// Restrict the hint to one tool, or `None` for every tool.
    pub tool_name: Option<String>,
    /// Case-insensitive substring of the error text that triggers the hint.
    pub error_pattern: String,
    /// Text appended after the `ERROR:` line of the tool message.
    pub hint: String,
}

/// The editable components around an agent's system prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Harness {
    /// Standing rules. Always-on.
    pub guardrails: Option<String>,
    /// Durable facts the host chose to keep in context (including lessons
    /// the host folded in). Always-on.
    pub memory: Option<String>,
    /// Override for the reflection executor's critique prompt.
    pub critique: Option<String>,
    /// On-demand procedures.
    pub skills: Vec<Skill>,
    /// Error-signature hints.
    pub tool_hints: Vec<ToolHint>,
}

impl Harness {
    /// True when nothing here changes a run (the pre-harness behaviour).
    pub fn is_empty(&self) -> bool {
        self.guardrails.is_none()
            && self.memory.is_none()
            && self.critique.is_none()
            && self.skills.is_empty()
            && self.tool_hints.is_empty()
    }

    /// The always-on block that follows the system prompt and soul:
    /// guardrails, memory, then one trigger line per skill. `None` when
    /// there is nothing to add.
    pub fn always_on_block(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(g) = non_blank(self.guardrails.as_deref()) {
            parts.push(g.to_owned());
        }
        if let Some(m) = non_blank(self.memory.as_deref()) {
            parts.push(m.to_owned());
        }
        if let Some(block) = self.skill_trigger_block() {
            parts.push(block);
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// The skill trigger lines, or `None` when the agent has no skills.
    pub fn skill_trigger_block(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut out = String::from(SKILL_TRIGGER_HEADER);
        for s in &self.skills {
            out.push_str("\n- When ");
            out.push_str(s.description.trim());
            out.push_str(", load the `");
            out.push_str(&s.name);
            out.push_str("` skill.");
        }
        Some(out)
    }

    /// Characters of always-on text this harness adds on top of the system
    /// prompt and soul.
    pub fn always_on_chars(&self) -> usize {
        self.always_on_block()
            .map(|b| b.chars().count())
            .unwrap_or(0)
    }

    /// Total always-on characters for a run, including the prompt and soul.
    pub fn total_always_on_chars(&self, system_prompt: &str, soul: Option<&str>) -> usize {
        system_prompt.chars().count()
            + soul.map(|s| s.chars().count()).unwrap_or(0)
            + self.always_on_chars()
    }

    /// The first hint whose pattern appears (case-insensitively) in `error`
    /// and whose `tool_name` is unset or equals `tool`. Hints are checked in
    /// the order the host supplied them.
    pub fn hint_for(&self, tool: &str, error: &str) -> Option<&str> {
        let err_lc = error.to_lowercase();
        self.tool_hints
            .iter()
            .filter(|h| h.tool_name.as_deref().is_none_or(|t| t == tool))
            .find(|h| {
                let pat = h.error_pattern.trim().to_lowercase();
                !pat.is_empty() && err_lc.contains(&pat)
            })
            .map(|h| h.hint.as_str())
    }

    /// Look up a skill body by name.
    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }
}

fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> Harness {
        Harness {
            guardrails: Some("Never inline literals into SQL.".into()),
            memory: Some("tickets.tier is one of free, pro, enterprise".into()),
            critique: None,
            skills: vec![Skill {
                name: "escalation".into(),
                description: "a ticket mentions an outage or data loss".into(),
                body: "1. Set priority=urgent ...".into(),
            }],
            tool_hints: vec![
                ToolHint {
                    tool_name: Some("sql_exec".into()),
                    error_pattern: "there is no parameter $".into(),
                    hint: "Count the $n placeholders; params must have exactly that many.".into(),
                },
                ToolHint {
                    tool_name: None,
                    error_pattern: "missing field `query`".into(),
                    hint: "Every SQL tool call needs a top-level \"query\" string.".into(),
                },
            ],
        }
    }

    #[test]
    fn empty_harness_adds_nothing() {
        let e = Harness::default();
        assert!(e.is_empty());
        assert!(e.always_on_block().is_none());
        assert_eq!(e.always_on_chars(), 0);
        assert!(e.hint_for("x", "anything").is_none());
    }

    #[test]
    fn always_on_block_orders_guardrails_memory_skills() {
        let b = h().always_on_block().unwrap();
        let g = b.find("Never inline").unwrap();
        let m = b.find("tickets.tier").unwrap();
        let s = b.find(SKILL_TRIGGER_HEADER).unwrap();
        assert!(g < m && m < s);
        assert!(b.contains(
            "- When a ticket mentions an outage or data loss, load the `escalation` skill."
        ));
    }

    #[test]
    fn blank_components_are_skipped() {
        let x = Harness {
            guardrails: Some("   ".into()),
            memory: Some("".into()),
            ..Default::default()
        };
        assert!(x.always_on_block().is_none());
    }

    #[test]
    fn hint_matches_tool_scoped_then_global() {
        let x = h();
        assert_eq!(
            x.hint_for(
                "sql_exec",
                "tool 'sql_exec' execution failed: there is no parameter $4"
            ),
            Some("Count the $n placeholders; params must have exactly that many.")
        );
        // Scoped hint does not fire for another tool; global one does.
        assert!(
            x.hint_for("sql_query", "there is no parameter $4")
                .is_none()
        );
        assert_eq!(
            x.hint_for(
                "sql_query",
                "invalid input for tool 'sql_query': MISSING FIELD `query`"
            ),
            Some("Every SQL tool call needs a top-level \"query\" string.")
        );
    }

    #[test]
    fn total_counts_prompt_and_soul() {
        let x = Harness::default();
        assert_eq!(x.total_always_on_chars("abc", Some("de")), 5);
    }

    #[test]
    fn serde_defaults_missing_fields() {
        let x: Harness = serde_json::from_str(r#"{"guardrails":"g"}"#).unwrap();
        assert_eq!(x.guardrails.as_deref(), Some("g"));
        assert!(x.skills.is_empty());
    }
}
