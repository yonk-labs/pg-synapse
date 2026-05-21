//! PS-5: wire AnthropicProvider to the shared cassette conformance suite.
//!
//! - `anthropic_static_conformance` (always runs, no network): asserts the
//!   real provider's `model_name` and PS-1 capabilities via `run_conformance`
//!   with a zero-entry cassette, so no `complete` call is made.
//! - `anthropic_live_record_then_replay` (feature `live-tests`, skips when
//!   `ANTHROPIC_API_KEY` is unset): wraps the real provider in
//!   `RecordingProvider`, does one live `complete`, then replays the
//!   recorded cassette through `run_conformance` to prove record/replay
//!   fidelity against a real provider.

use pg_synapse_core::llm::ProviderCapabilities;
use pg_synapse_core::testing::{Cassette, CassetteProvider, run_conformance};
use pg_synapse_provider_anthropic::AnthropicProvider;

/// AnthropicProvider's advertised capabilities (mirrors `impl LlmProvider`).
/// Keep this in lockstep with `client.rs::capabilities`.
fn anthropic_caps() -> ProviderCapabilities {
    ProviderCapabilities {
        tool_use: true,
        streaming: true,
        json_mode: true,
        vision: true,
        max_context_tokens: None,
        max_output_tokens: None,
    }
}

#[tokio::test]
async fn anthropic_static_conformance() {
    let provider = AnthropicProvider::new("conformance-model", "http://unused.invalid");
    let expected = Cassette {
        model: "conformance-model".into(),
        capabilities: anthropic_caps(),
        entries: vec![], // no entries: model + capabilities only, no network
    };
    run_conformance(&provider, &expected)
        .await
        .expect("AnthropicProvider must satisfy its declared PS-1 contract");
}

/// PS-5 slice 3c: hermetic regression check against a committed golden
/// cassette. Catches silent serde-shape changes; exercises CassetteProvider
/// replay against a non-empty entries list (the static test uses zero).
#[tokio::test]
async fn anthropic_golden_cassette_replays() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/conformance-default.json");
    let cassette = Cassette::load(&path).expect("golden cassette must load");
    assert_eq!(cassette.model, "conformance-model");
    assert_eq!(
        cassette.entries.len(),
        1,
        "fixture exercises one interaction"
    );
    let replay = CassetteProvider::new(cassette);
    let expected = Cassette::load(&path).expect("golden cassette must load");
    run_conformance(&replay, &expected)
        .await
        .expect("golden cassette must replay cleanly");
}

#[cfg(feature = "live-tests")]
fn api_key_and_model() -> Option<(String, String)> {
    let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    let model = std::env::var("PG_SYNAPSE_TEST_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-3-5-haiku-20241022".to_string());
    Some((key, model))
}

#[cfg(feature = "live-tests")]
#[tokio::test]
async fn anthropic_live_record_then_replay() {
    use pg_synapse_core::LlmProvider;
    use pg_synapse_core::testing::RecordingProvider;
    use pg_synapse_core::types::{CompletionRequest, Message, Role};
    use std::sync::Arc;

    let Some((key, model)) = api_key_and_model() else {
        eprintln!("skipping: ANTHROPIC_API_KEY unset");
        return;
    };

    let real =
        Arc::new(AnthropicProvider::new(&model, "https://api.anthropic.com").with_api_key(key));
    let recorder = RecordingProvider::new(real);

    let req = CompletionRequest {
        messages: vec![Message {
            execution_id: uuid::Uuid::new_v4(),
            seq: 0,
            role: Role::User,
            content: Some("Reply with the single word: pong".into()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: chrono::Utc::now(),
        }],
        ..Default::default()
    };
    recorder
        .complete(req)
        .await
        .expect("live anthropic complete should succeed");

    // Snapshot once (into_cassette drains), then derive both the replay
    // provider and the expected cassette from the same serialized form so
    // the conformance check is meaningful and needs no Clone.
    let json = recorder.into_cassette().to_json();
    let expected = Cassette::from_json(&json).expect("recorded cassette parses");
    assert_eq!(expected.entries.len(), 1, "one interaction recorded");
    let replay = CassetteProvider::from_json(&json).expect("recorded cassette parses");
    run_conformance(&replay, &expected)
        .await
        .expect("recorded cassette must replay cleanly");
}
