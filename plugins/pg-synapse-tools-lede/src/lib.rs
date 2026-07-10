//! Lede compression tool plugin for pg_synapse: `lede_compress`.
//!
//! ## What this does
//!
//! `lede_compress` takes a block of text and a token budget and returns an
//! executive brief. It is a documented SHIM: real lede integration is deferred
//! to v0.2. Two code paths:
//!
//! 1. **lede CLI, opt-in:** when `PG_SYNAPSE_LEDE_CLI` is set to `1` or `true`
//!    AND a `lede` binary is on PATH, shells to `lede --max-chars <n>`, feeds
//!    `text` on stdin, captures stdout. Subprocess errors are wrapped in
//!    [`ToolError::Execution`]. The char budget is estimated from the token
//!    budget as `max_tokens * 4`. This path is opt-in because `lede`'s CLI is
//!    an external contract that can drift; auto-detecting it made behavior
//!    depend on host state and broke hermetic tests.
//!
//! 2. **Default (shim):** deterministic extractive compression
//!    in pure Rust. Sentences are split on `. ! ?` boundaries, scored by
//!    length-normalized keyword salience (words >4 chars that appear more than
//!    once in the full text), then selected greedily in original order until
//!    the approximate token budget (words * 1.3) is reached.
//!
//! ## Arg-alias leniency
//!
//! Models frequently name tool arguments differently from the canonical field
//! names. `text` accepts aliases `content` and `input`. `max_tokens` accepts
//! aliases `budget`, `limit`, and `max`. The JSON schema still advertises only
//! the canonical names.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::OnceLock;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use schemars::JsonSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

// ---------------------------------------------------------------------------
// Schema builder helper
// ---------------------------------------------------------------------------

fn build_schema<T: JsonSchema>() -> ToolSchema {
    let root = schema_for!(T);
    let val = serde_json::to_value(&root).expect("schemars output is always valid JSON");
    ToolSchema::from_json_value(val).expect("schemars schema is always a valid object")
}

// ---------------------------------------------------------------------------
// Input struct with lenient aliases
// ---------------------------------------------------------------------------

/// Input schema for `lede_compress`.
///
/// Canonical field: `text`. Accepted aliases: `content`, `input`.
/// Canonical field: `max_tokens`. Accepted aliases: `budget`, `limit`, `max`.
#[derive(Deserialize, JsonSchema, Debug)]
struct LedeCompressInput {
    /// The text to compress into an executive brief.
    #[serde(alias = "content", alias = "input")]
    text: String,

    /// Approximate token budget for the output brief. Tokens estimated as
    /// words * 1.3. Defaults to 200.
    #[serde(
        default = "default_max_tokens",
        alias = "budget",
        alias = "limit",
        alias = "max"
    )]
    max_tokens: u32,
}

fn default_max_tokens() -> u32 {
    200
}

// ---------------------------------------------------------------------------
// Extractive compression (shim path)
// ---------------------------------------------------------------------------

/// Split text into sentences on `. ! ?` boundaries (trailing punctuation stays
/// with the preceding sentence). Returns a Vec of trimmed, non-empty strings.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        current.push(ch);
        if ch == '.' || ch == '!' || ch == '?' {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }

    // Trailing fragment without terminal punctuation.
    let tail = current.trim().to_string();
    if !tail.is_empty() {
        sentences.push(tail);
    }

    sentences
}

/// Tokenize text into lowercase words, stripping non-alphanumeric characters.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Build a frequency map over words.
fn word_freq(words: &[String]) -> std::collections::HashMap<String, usize> {
    let mut freq = std::collections::HashMap::new();
    for w in words {
        *freq.entry(w.clone()).or_insert(0) += 1;
    }
    freq
}

/// Score a sentence by length-normalized keyword salience. Keywords are words
/// longer than 4 chars that appear more than once in the full-text frequency
/// table.
fn score_sentence(sentence: &str, freq: &std::collections::HashMap<String, usize>) -> f64 {
    let words = tokenize(sentence);
    if words.is_empty() {
        return 0.0;
    }
    let keyword_count: usize = words
        .iter()
        .filter(|w| w.len() > 4 && freq.get(*w).copied().unwrap_or(0) > 1)
        .count();
    keyword_count as f64 / words.len() as f64
}

/// Estimate token count as words * 1.3.
fn estimate_tokens(text: &str) -> f64 {
    let word_count = text.split_whitespace().count();
    word_count as f64 * 1.3
}

/// Deterministic extractive compression. Selects highest-scoring sentences
/// in original order until the token budget is approximately satisfied.
///
/// Always returns a non-empty string. If the full input fits in the budget
/// (or has only one sentence) it is returned unchanged.
fn extractive_compress(text: &str, max_tokens: u32) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let sentences = split_sentences(trimmed);

    // Single sentence or budget already satisfied: return as-is.
    if sentences.len() <= 1 || estimate_tokens(trimmed) <= max_tokens as f64 {
        return trimmed.to_string();
    }

    // Build global frequency table.
    let all_words = tokenize(trimmed);
    let freq = word_freq(&all_words);

    // Score each sentence.
    let mut scored: Vec<(usize, f64)> = sentences
        .iter()
        .enumerate()
        .map(|(i, s)| (i, score_sentence(s, &freq)))
        .collect();

    // Sort by score descending, then by original index ascending (stable tiebreak).
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });

    // Greedily pick sentences until budget is reached.
    let mut selected_indices: Vec<usize> = Vec::new();
    let mut token_total = 0.0_f64;

    for (idx, _score) in &scored {
        let sentence_tokens = estimate_tokens(&sentences[*idx]);
        if token_total + sentence_tokens > max_tokens as f64 && !selected_indices.is_empty() {
            break;
        }
        selected_indices.push(*idx);
        token_total += sentence_tokens;
    }

    // Restore original order.
    selected_indices.sort_unstable();

    // Guarantee at least one sentence.
    if selected_indices.is_empty() {
        selected_indices.push(0);
    }

    selected_indices
        .iter()
        .map(|i| sentences[*i].as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// lede_compress tool
// ---------------------------------------------------------------------------

/// Tool: compress text into a token-bounded executive brief.
///
/// Uses the `lede` CLI if available on PATH; otherwise falls back to a
/// deterministic extractive compression shim.
pub struct LedeCompressTool {
    schema: OnceLock<ToolSchema>,
}

impl LedeCompressTool {
    fn new() -> Self {
        Self {
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for LedeCompressTool {
    fn name(&self) -> &str {
        "lede_compress"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<LedeCompressInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: LedeCompressInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "lede_compress".into(),
                reason: e.to_string(),
            })?;

        let text = args.text.trim().to_string();
        let max_tokens = args.max_tokens;

        debug!(
            "lede_compress: max_tokens={} input_chars={}",
            max_tokens,
            text.len()
        );

        let input_chars = text.len();

        // The CLI path is opt-in: `lede`'s CLI is an external contract that can
        // drift, so auto-detecting it would make behavior depend on host state.
        // Only use it when explicitly enabled AND the binary is present.
        let use_cli = matches!(
            std::env::var("PG_SYNAPSE_LEDE_CLI").as_deref(),
            Ok("1") | Ok("true")
        ) && std::process::Command::new("which")
            .arg("lede")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if use_cli {
            // Shell to lede CLI. `lede` takes a character budget, not tokens;
            // estimate chars from the token budget (~4 chars per token).
            use std::io::Write;
            let max_chars = max_tokens.saturating_mul(4);
            let mut child = std::process::Command::new("lede")
                .arg("--max-chars")
                .arg(max_chars.to_string())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| ToolError::Execution {
                    name: "lede_compress".into(),
                    reason: format!("failed to spawn lede: {e}"),
                })?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(text.as_bytes())
                    .map_err(|e| ToolError::Execution {
                        name: "lede_compress".into(),
                        reason: format!("failed to write to lede stdin: {e}"),
                    })?;
            }

            let output = child.wait_with_output().map_err(|e| ToolError::Execution {
                name: "lede_compress".into(),
                reason: format!("lede process error: {e}"),
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ToolError::Execution {
                    name: "lede_compress".into(),
                    reason: format!("lede exited non-zero: {stderr}"),
                });
            }

            let brief = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let brief_chars = brief.len();

            return Ok(ToolOutput::Json(serde_json::json!({
                "brief": brief,
                "source": "lede-cli",
                "input_chars": input_chars,
                "brief_chars": brief_chars,
            })));
        }

        // Extractive shim path.
        let brief = extractive_compress(&text, max_tokens);
        let brief_chars = brief.len();

        Ok(ToolOutput::Json(serde_json::json!({
            "brief": brief,
            "source": "extractive-shim",
            "input_chars": input_chars,
            "brief_chars": brief_chars,
        })))
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Plugin that registers `lede_compress` into a host [`Registry`].
pub struct LedeToolsPlugin;

impl LedeToolsPlugin {
    /// Create the plugin. No configuration required.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LedeToolsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for LedeToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-lede"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry.tools.add_arc(
            "lede_compress",
            std::sync::Arc::new(LedeCompressTool::new()),
        );
    }
}
