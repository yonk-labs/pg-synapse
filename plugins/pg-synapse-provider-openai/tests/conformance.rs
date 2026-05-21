//! PS-5: wire OpenAiProvider to the shared cassette conformance suite.
//!
//! - `openai_static_conformance` (always runs, no network): asserts the real
//!   provider's model_name + PS-1 capabilities via `run_conformance` with a
//!   zero-entry cassette, so no `complete` call is made.
//! - `openai_live_record_then_replay` (feature `live-tests`, skips when
//!   `PG_SYNAPSE_TEST_LLM_BASE_URL` is unset): wraps the real provider in
//!   `RecordingProvider`, does one live `complete`, then replays the
//!   recorded cassette through `run_conformance` to prove record/replay
//!   fidelity against a real provider.

use pg_synapse_core::llm::ProviderCapabilities;
use pg_synapse_core::testing::{
    Cassette, CassetteProvider, default_conformance_cassette, run_conformance,
};
use pg_synapse_provider_openai::OpenAiProvider;

/// Path to this crate's committed golden cassette.
fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance-default.json")
}

/// OpenAiProvider's advertised capabilities (mirrors `impl LlmProvider`).
fn openai_caps() -> ProviderCapabilities {
    ProviderCapabilities {
        tool_use: true,
        streaming: true,
        json_mode: true,
        vision: false,
        max_context_tokens: None,
        max_output_tokens: None,
    }
}

#[tokio::test]
async fn openai_static_conformance() {
    let provider = OpenAiProvider::new("conformance-model", "http://unused.invalid/v1");
    let expected = Cassette {
        model: "conformance-model".into(),
        capabilities: openai_caps(),
        entries: vec![], // no entries: model + capabilities only, no network
    };
    run_conformance(&provider, &expected)
        .await
        .expect("OpenAiProvider must satisfy its declared PS-1 contract");
}

/// PS-5 slice 3a: hermetic regression check against a committed golden
/// cassette. Catches silent serde-shape changes in the Cassette /
/// CompletionRequest / CompletionResponse / CassetteOutcome types: if any
/// of those evolve, loading the committed fixture will fail at parse time.
/// Also exercises CassetteProvider replay through `run_conformance` against
/// a non-empty entries list (the static test uses zero entries).
#[tokio::test]
async fn openai_golden_cassette_replays() {
    let path = fixture_path();
    let cassette = Cassette::load(&path).expect("golden cassette must load");
    assert_eq!(cassette.model, "conformance-model");
    assert_eq!(
        cassette.entries.len(),
        3,
        "fixture exercises text reply + tool call + error variant"
    );
    let replay = CassetteProvider::new(cassette);
    let expected = Cassette::load(&path).expect("golden cassette must load");
    run_conformance(&replay, &expected)
        .await
        .expect("golden cassette must replay cleanly");
}

/// PS-5 slice 4: drift check. The committed fixture must equal the canonical
/// to_json output of `default_conformance_cassette` for this provider's
/// capabilities. If they diverge (someone hand-edited the JSON, or a serde
/// shape changed) this test fails; run `regenerate_openai_golden_cassette`
/// to bring them back in sync.
#[test]
fn openai_golden_cassette_matches_canonical() {
    let canonical = default_conformance_cassette("conformance-model", openai_caps()).to_json();
    let committed = std::fs::read_to_string(fixture_path()).expect("fixture must exist");
    assert_eq!(
        canonical,
        committed.trim_end_matches('\n'),
        "fixture drift: run `cargo test -p pg-synapse-provider-openai --test conformance \
         regenerate_openai_golden_cassette -- --ignored` to refresh"
    );
}

/// Regenerator (always ignored). Run explicitly when the canonical shape
/// changes: `cargo test ... regenerate_openai_golden_cassette -- --ignored`.
#[test]
#[ignore]
fn regenerate_openai_golden_cassette() {
    let canonical = default_conformance_cassette("conformance-model", openai_caps()).to_json();
    let mut bytes = canonical.into_bytes();
    bytes.push(b'\n');
    std::fs::write(fixture_path(), bytes).expect("fixture is writable");
}

#[cfg(feature = "live-tests")]
fn endpoint() -> Option<(String, String)> {
    let base = std::env::var("PG_SYNAPSE_TEST_LLM_BASE_URL").ok()?;
    let model = std::env::var("PG_SYNAPSE_TEST_LLM_MODEL")
        .unwrap_or_else(|_| "granite-3.1-2b-instruct".to_string());
    Some((base, model))
}

#[cfg(feature = "live-tests")]
#[tokio::test]
async fn openai_live_record_then_replay() {
    use pg_synapse_core::LlmProvider;
    use pg_synapse_core::testing::RecordingProvider;
    use pg_synapse_core::types::{CompletionRequest, Message, Role};
    use std::sync::Arc;

    let Some((base, model)) = endpoint() else {
        eprintln!("skipping: PG_SYNAPSE_TEST_LLM_BASE_URL unset");
        return;
    };

    let real = Arc::new(OpenAiProvider::new(&model, &base));
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
        .expect("live openai complete should succeed");

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
