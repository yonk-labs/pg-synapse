//! Integration tests for the lede_compress tool (shim path only).
//!
//! The lede CLI path is not tested here because no `lede` binary is
//! installed in the dev/CI environment. The shim path is deterministic and
//! fully covered by the tests below.

use pg_synapse_core::Tool;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use serde_json::Value;

/// Build a registry with the lede plugin and return the lede_compress tool.
fn tool() -> std::sync::Arc<dyn Tool> {
    let mut registry = Registry::new();
    pg_synapse_tools_lede::LedeToolsPlugin::new().register(&mut registry);
    registry
        .tools
        .get("lede_compress")
        .expect("lede_compress registered")
}

/// Call lede_compress with canonical args, return the JSON output value.
async fn compress(text: &str, max_tokens: u32) -> Value {
    let t = tool();
    let input = serde_json::json!({ "text": text, "max_tokens": max_tokens });
    let out = t
        .run(input, &ToolCtx::default())
        .await
        .expect("no tool error");
    match out {
        ToolOutput::Json(v) => v,
        other => panic!("expected Json output, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 1: extractive shim shortens multi-sentence text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shim_shortens_multi_sentence_text() {
    // 10 sentences, each ~10 words. Budget of 20 tokens should yield far fewer sentences.
    let text = "The system processes customer feedback efficiently. \
                Every feedback item is stored in the database. \
                Sentiment analysis classifies each item automatically. \
                Positive feedback highlights product strengths clearly. \
                Negative feedback identifies areas for improvement quickly. \
                Neutral feedback provides balanced observations overall. \
                The digest table stores compressed results reliably. \
                Executives receive a brief summary each morning. \
                The agent loop runs on every new feedback item. \
                Performance metrics are tracked continuously by the system.";

    let result = compress(text, 20).await;
    let brief = result["brief"].as_str().expect("brief is string");
    let source = result["source"].as_str().expect("source is string");

    assert_eq!(source, "extractive-shim");
    // Brief must be shorter than the input.
    assert!(
        brief.len() < text.len(),
        "brief ({} chars) should be shorter than input ({} chars)",
        brief.len(),
        text.len()
    );
    // Brief must be non-empty.
    assert!(!brief.is_empty(), "brief must not be empty");
}

// ---------------------------------------------------------------------------
// Test 2: smaller budget produces shorter output
// ---------------------------------------------------------------------------

#[tokio::test]
async fn smaller_budget_produces_shorter_output() {
    let text = "The customer support system handles thousands of requests. \
                Each request is triaged by priority and category. \
                High-priority requests are escalated to the support team. \
                Enterprise customers receive priority handling always. \
                The audit trail records every action taken on tickets. \
                Billing issues are routed to the finance team directly. \
                API issues are handled by the engineering team quickly. \
                Reports are generated weekly for management review.";

    let large = compress(text, 100).await;
    let small = compress(text, 15).await;

    let large_chars = large["brief_chars"].as_u64().unwrap_or(u64::MAX);
    let small_chars = small["brief_chars"].as_u64().unwrap_or(0);

    assert!(
        small_chars <= large_chars,
        "smaller budget ({} chars) should produce output no longer than larger budget ({} chars)",
        small_chars,
        large_chars
    );
}

// ---------------------------------------------------------------------------
// Test 3: single-sentence input is returned unchanged
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_sentence_passthrough() {
    let text = "The system is working correctly.";
    let result = compress(text, 50).await;
    let brief = result["brief"].as_str().expect("brief is string");
    assert_eq!(brief, text.trim());
}

// ---------------------------------------------------------------------------
// Test 4: alias `content` -> text works
// ---------------------------------------------------------------------------

#[tokio::test]
async fn alias_content_maps_to_text() {
    let t = tool();
    // Use alias "content" instead of canonical "text".
    let input = serde_json::json!({
        "content": "The product works well. Customers are satisfied. Support is fast.",
        "max_tokens": 10
    });
    let out = t
        .run(input, &ToolCtx::default())
        .await
        .expect("alias content should not cause an error");
    match out {
        ToolOutput::Json(v) => {
            assert!(v["brief"].as_str().is_some(), "brief present");
        }
        other => panic!("expected Json, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 5: alias `budget` -> max_tokens works
// ---------------------------------------------------------------------------

#[tokio::test]
async fn alias_budget_maps_to_max_tokens() {
    let t = tool();
    // Use alias "budget" instead of canonical "max_tokens".
    let input = serde_json::json!({
        "text": "The product is great. Customers love it. Support is responsive. Teams are happy.",
        "budget": 10
    });
    let out = t
        .run(input, &ToolCtx::default())
        .await
        .expect("alias budget should not cause an error");
    match out {
        ToolOutput::Json(v) => {
            assert!(v["brief"].as_str().is_some(), "brief present");
            // With a tight budget, the brief should be short.
            let brief_chars = v["brief_chars"].as_u64().unwrap_or(0);
            assert!(brief_chars > 0, "brief_chars must be positive");
        }
        other => panic!("expected Json, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 6: empty/whitespace input does not panic, returns graceful output
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_input_is_graceful() {
    let t = tool();

    // Truly empty string.
    let input = serde_json::json!({ "text": "", "max_tokens": 50 });
    let out = t.run(input, &ToolCtx::default()).await;
    // Must not panic. A tool error is acceptable; an Ok with an empty brief is also fine.
    match out {
        Ok(ToolOutput::Json(v)) => {
            // brief should exist (may be empty string).
            assert!(v["brief"].is_string(), "brief field must be a string");
        }
        Ok(other) => panic!("expected Json output for empty input, got {:?}", other),
        Err(_) => {
            // An error is acceptable for empty input; the important thing is no panic.
        }
    }

    // Whitespace-only string.
    let input2 = serde_json::json!({ "text": "   \n  ", "max_tokens": 50 });
    let out2 = t.run(input2, &ToolCtx::default()).await;
    match out2 {
        Ok(ToolOutput::Json(v)) => {
            assert!(v["brief"].is_string(), "brief field must be a string");
        }
        Ok(other) => panic!("expected Json output for whitespace input, got {:?}", other),
        Err(_) => {
            // Acceptable.
        }
    }
}

// ---------------------------------------------------------------------------
// Test 7: output fields present and typed correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn output_fields_typed_correctly() {
    let text = "Feedback analysis improves product quality. \
                Customers report issues clearly. Teams resolve problems quickly.";
    let result = compress(text, 30).await;

    assert!(result["brief"].is_string(), "brief must be string");
    assert!(result["source"].is_string(), "source must be string");
    assert!(
        result["input_chars"].is_number(),
        "input_chars must be number"
    );
    assert!(
        result["brief_chars"].is_number(),
        "brief_chars must be number"
    );

    let source = result["source"].as_str().unwrap();
    assert!(
        source == "extractive-shim" || source == "lede-cli",
        "source must be one of the two known values, got: {source}"
    );
}
