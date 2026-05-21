//! PS-5: wire LlamaCppProvider to the shared cassette conformance suite.
//!
//! - `llamacpp_static_conformance` (always runs, no network): asserts the
//!   real provider's `model_name` and PS-1 capabilities via `run_conformance`
//!   with a zero-entry cassette, so no `complete` call is made.
//! - `llamacpp_live_record_then_replay` (feature `live-tests`, skips when
//!   `PG_SYNAPSE_TEST_LLAMACPP_BASE_URL` is unset): wraps the real provider
//!   in `RecordingProvider`, does one live `complete`, then replays the
//!   recorded cassette through `run_conformance` to prove record/replay
//!   fidelity against a real provider.

use pg_synapse_core::llm::ProviderCapabilities;
use pg_synapse_core::testing::{Cassette, run_conformance};
use pg_synapse_provider_llama_cpp::LlamaCppProvider;

/// LlamaCppProvider does not override `LlmProvider::capabilities`, so its
/// advertised contract is the default (all false / None). If the provider
/// later declares concrete capabilities, mirror them here.
fn llamacpp_caps() -> ProviderCapabilities {
    ProviderCapabilities::default()
}

#[tokio::test]
async fn llamacpp_static_conformance() {
    let provider = LlamaCppProvider::new("conformance-model", "http://unused.invalid/v1");
    let expected = Cassette {
        model: "conformance-model".into(),
        capabilities: llamacpp_caps(),
        entries: vec![], // no entries: model + capabilities only, no network
    };
    run_conformance(&provider, &expected)
        .await
        .expect("LlamaCppProvider must satisfy its declared PS-1 contract");
}

#[cfg(feature = "live-tests")]
fn endpoint() -> Option<(String, String)> {
    let base = std::env::var("PG_SYNAPSE_TEST_LLAMACPP_BASE_URL").ok()?;
    let model = std::env::var("PG_SYNAPSE_TEST_LLAMACPP_MODEL")
        .unwrap_or_else(|_| "granite-3.0-2b-instruct".to_string());
    Some((base, model))
}

#[cfg(feature = "live-tests")]
#[tokio::test]
async fn llamacpp_live_record_then_replay() {
    use pg_synapse_core::LlmProvider;
    use pg_synapse_core::testing::{CassetteProvider, RecordingProvider};
    use pg_synapse_core::types::{CompletionRequest, Message, Role};
    use std::sync::Arc;

    let Some((base, model)) = endpoint() else {
        eprintln!("skipping: PG_SYNAPSE_TEST_LLAMACPP_BASE_URL unset");
        return;
    };

    let real = Arc::new(LlamaCppProvider::new(&model, &base));
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
        .expect("live llama-cpp complete should succeed");

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
