//! Cassette record/replay + conformance harness (PS-5).
//!
//! A [`Cassette`] is an ordered recording of provider interactions plus the
//! provider's declared model and [`ProviderCapabilities`] (the PS-1
//! capability model). [`CassetteProvider`] replays one as an
//! [`LlmProvider`] with no network, so provider plugins get a deterministic
//! conformance test and live-dependent tests can be de-flaked.
//!
//! Replay is sequential (FIFO), matching [`super::MockLlmProvider`]'s queue
//! convention rather than fragile request matching. This is dev/test tooling
//! only: nothing here is on the runtime-core default path (G4).
//!
//! ## Pieces
//!
//! * [`CassetteProvider`] + [`run_conformance`] replay a recorded cassette
//!   and assert a provider reproduces its model, capabilities, and
//!   outcomes (slice 1).
//! * [`RecordingProvider`] wraps a real [`LlmProvider`] and captures every
//!   `complete` call into a cassette (slice 2a). [`Cassette::save`] /
//!   [`Cassette::load`] are the on-disk format.
//! * [`default_conformance_cassette`] is the canonical three-entry
//!   cassette (text reply + tool call + auth error) used by every wired
//!   provider plugin as the source of truth for its committed golden
//!   fixture (slices 4 and 5). Plugins' drift checks pin the committed
//!   JSON to this helper's `to_json` output; their `#[ignore]`
//!   regenerator tests bring fixtures back into sync when the canonical
//!   shape evolves.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::llm::{LlmProvider, ProviderCapabilities};
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse};
use crate::types::{Message, Role, ToolCall, Usage};

/// The recorded result of one `complete` call.
#[derive(Debug, Serialize, Deserialize)]
pub enum CassetteOutcome {
    /// A successful completion.
    Ok(CompletionResponse),
    /// A provider error (`LlmError` is serde but not `Clone`, so replay
    /// re-deserializes a fresh value per call).
    Err(LlmError),
}

/// One recorded request/outcome pair.
#[derive(Debug, Serialize, Deserialize)]
pub struct CassetteEntry {
    /// The request that produced `outcome`.
    pub request: CompletionRequest,
    /// What the provider returned.
    pub outcome: CassetteOutcome,
}

/// An ordered recording of provider interactions plus the provider's
/// declared identity and capabilities.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cassette {
    /// The recorded provider's default model name.
    pub model: String,
    /// The recorded provider's advertised capabilities (PS-1).
    pub capabilities: ProviderCapabilities,
    /// Interactions, replayed in order.
    pub entries: Vec<CassetteEntry>,
}

impl Cassette {
    /// Pretty JSON, the on-disk cassette format.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("Cassette is always serializable")
    }

    /// Parse a cassette from its JSON form.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Write the cassette to `path` as pretty JSON.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        std::fs::write(path, self.to_json())
    }

    /// Read a cassette from `path`. A malformed file surfaces as
    /// [`std::io::ErrorKind::InvalidData`].
    pub fn load(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_json(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Build the canonical three-entry conformance cassette: the single source
/// of truth for the per-provider golden fixtures (PS-5 slice 4 + 5). The
/// shape is deliberately minimal and deterministic; all messages use
/// `Uuid::nil()` and the unix epoch as `timestamp`. Provider plugins
/// parameterize only `model` and `capabilities`; the entries are identical
/// across providers so any divergence is a serde-shape change in the
/// shared types, not noise.
///
/// The three entries pin three distinct CassetteOutcome / response shapes
/// that real providers must reproduce in replay:
/// 1. Text reply (`Ok` with `content` set, empty `tool_calls`).
/// 2. Tool call (`Ok` with `content = None`, one `ToolCall`).
/// 3. Auth error (`Err(LlmError::Auth)`).
pub fn default_conformance_cassette(
    model: impl Into<String>,
    capabilities: ProviderCapabilities,
) -> Cassette {
    let user_msg = |content: &str| Message {
        execution_id: uuid::Uuid::nil(),
        seq: 0,
        role: Role::User,
        content: Some(content.into()),
        tool_call_id: None,
        tool_name: None,
        tool_input: None,
        tool_output: None,
        timestamp: chrono::DateTime::from_timestamp(0, 0).expect("unix epoch is a valid timestamp"),
    };
    let entry = |req_content: &str, outcome: CassetteOutcome| CassetteEntry {
        request: CompletionRequest {
            messages: vec![user_msg(req_content)],
            ..Default::default()
        },
        outcome,
    };

    // Entry 1 carries non-None Option<f32> / Option<u32> on the request and
    // non-zero Usage including Option<f64> cost on the response, so the
    // golden fixture pins the JSON shape of those non-default Options in
    // addition to the None defaults pinned by entries 2 and 3.
    let pinged_request = CompletionRequest {
        messages: vec![user_msg("ping")],
        tools: vec![],
        model: None,
        temperature: Some(0.0),
        max_tokens: Some(32),
        params: serde_json::Value::Null,
    };

    Cassette {
        model: model.into(),
        capabilities,
        entries: vec![
            CassetteEntry {
                request: pinged_request,
                outcome: CassetteOutcome::Ok(CompletionResponse {
                    content: Some("pong".into()),
                    tool_calls: vec![],
                    finish_reason: "stop".into(),
                    usage: Usage {
                        tokens_in: 2,
                        tokens_out: 1,
                        cost_usd: Some(0.001),
                    },
                }),
            },
            entry(
                "use the echo tool with input ping",
                CassetteOutcome::Ok(CompletionResponse {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "call_0".into(),
                        name: "echo".into(),
                        args: serde_json::json!({ "input": "ping" }),
                    }],
                    finish_reason: "tool_calls".into(),
                    usage: Usage::default(),
                }),
            ),
            entry(
                "trigger auth failure",
                CassetteOutcome::Err(LlmError::Auth("conformance-provider".into())),
            ),
        ],
    }
}

/// Externally-tagged borrowed mirror of [`CassetteOutcome`]. Serde gives a
/// reference enum the same JSON as the owned one, so this lets
/// [`RecordingProvider`] serialize an outcome it must also return without
/// requiring `Clone` on [`LlmError`].
#[derive(Serialize)]
enum CassetteOutcomeRef<'a> {
    Ok(&'a CompletionResponse),
    Err(&'a LlmError),
}

/// Wraps a real [`LlmProvider`], passing every `complete` call through while
/// recording the request and outcome into a [`Cassette`]. Drive it against a
/// live endpoint (gated like live-tests) to regenerate fixtures; `stream`
/// delegates without recording (slice 1 replay is complete-only).
pub struct RecordingProvider {
    inner: std::sync::Arc<dyn LlmProvider>,
    recorded: std::sync::Mutex<Vec<CassetteEntry>>,
}

impl RecordingProvider {
    /// Wrap `inner` and start recording.
    pub fn new(inner: std::sync::Arc<dyn LlmProvider>) -> Self {
        Self {
            inner,
            recorded: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Snapshot everything recorded so far into a replayable cassette,
    /// carrying the inner provider's model and capabilities (PS-1).
    pub fn into_cassette(&self) -> Cassette {
        let entries = self
            .recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .collect();
        Cassette {
            model: self.inner.model_name().to_string(),
            capabilities: self.inner.capabilities(),
            entries,
        }
    }
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let result = self.inner.complete(req.clone()).await;

        // Serialize from a borrowed view so we never move the outcome out of
        // `result` (LlmError is not Clone), then re-parse into an owned
        // CassetteOutcome for storage.
        let outcome_ref = match &result {
            Ok(resp) => CassetteOutcomeRef::Ok(resp),
            Err(err) => CassetteOutcomeRef::Err(err),
        };
        let owned: CassetteOutcome = serde_json::from_value(
            serde_json::to_value(&outcome_ref).expect("CassetteOutcome is always serializable"),
        )
        .expect("a CassetteOutcome we serialized always re-parses");
        self.recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(CassetteEntry {
                request: req,
                outcome: owned,
            });

        result
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        self.inner.stream(req).await
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}

/// Replays a [`Cassette`] as an [`LlmProvider`], deterministically and
/// without network. Outcomes are stored pre-serialized so replay needs no
/// `Clone` on `LlmError`.
pub struct CassetteProvider {
    model: String,
    capabilities: ProviderCapabilities,
    outcomes: Vec<String>,
    cursor: AtomicUsize,
}

impl CassetteProvider {
    /// Build a replay provider from a cassette.
    pub fn new(cassette: Cassette) -> Self {
        let outcomes = cassette
            .entries
            .iter()
            .map(|e| {
                serde_json::to_string(&e.outcome).expect("CassetteOutcome is always serializable")
            })
            .collect();
        Self {
            model: cassette.model,
            capabilities: cassette.capabilities,
            outcomes,
            cursor: AtomicUsize::new(0),
        }
    }

    /// Build a replay provider from cassette JSON.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        Ok(Self::new(Cassette::from_json(s)?))
    }
}

#[async_trait]
impl LlmProvider for CassetteProvider {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let i = self.cursor.fetch_add(1, Ordering::SeqCst);
        let raw = self.outcomes.get(i).ok_or_else(|| LlmError::Provider {
            provider: "cassette".into(),
            reason: format!("cassette exhausted: no entry for call #{i}"),
        })?;
        // Re-deserialize per call: LlmError is not Clone, so the stored
        // outcome is the single source of truth and yields a fresh value.
        match serde_json::from_str::<CassetteOutcome>(raw)
            .expect("a CassetteOutcome we serialized always re-parses")
        {
            CassetteOutcome::Ok(resp) => Ok(resp),
            CassetteOutcome::Err(err) => Err(err),
        }
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        Err(LlmError::Provider {
            provider: "cassette".into(),
            reason: "streaming replay is not supported (slice 1 is complete-only)".into(),
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }
}

/// Why a provider failed the conformance suite.
#[derive(Debug, thiserror::Error)]
pub enum ConformanceError {
    /// `model_name()` did not match the cassette.
    #[error("model mismatch: provider reported {got:?}, cassette has {expected:?}")]
    ModelMismatch {
        /// What the provider reported.
        got: String,
        /// What the cassette declared.
        expected: String,
    },
    /// `capabilities()` did not match the cassette (PS-1).
    #[error("capability mismatch vs cassette")]
    CapabilityMismatch,
    /// An interaction's outcome class differed (ok vs err, or content).
    #[error("outcome mismatch at entry {index}: {detail}")]
    OutcomeMismatch {
        /// Zero-based entry index.
        index: usize,
        /// Human-readable difference.
        detail: String,
    },
}

/// Drive `provider` through `expected`'s requests in order and assert it
/// reproduces the recorded identity, capabilities (PS-1), and outcomes.
/// This is the reusable suite every `LlmProvider` impl can run against a
/// committed cassette with no network.
pub async fn run_conformance(
    provider: &dyn LlmProvider,
    expected: &Cassette,
) -> Result<(), ConformanceError> {
    if provider.model_name() != expected.model {
        return Err(ConformanceError::ModelMismatch {
            got: provider.model_name().to_string(),
            expected: expected.model.clone(),
        });
    }

    // ProviderCapabilities has no PartialEq; compare via serde_json::Value
    // (which does) so the PS-1 capability contract is checked structurally.
    let got_caps = serde_json::to_value(provider.capabilities())
        .expect("ProviderCapabilities is always serializable");
    let want_caps = serde_json::to_value(&expected.capabilities)
        .expect("ProviderCapabilities is always serializable");
    if got_caps != want_caps {
        return Err(ConformanceError::CapabilityMismatch);
    }

    for (index, entry) in expected.entries.iter().enumerate() {
        let got = provider.complete(entry.request.clone()).await;
        match (&entry.outcome, got) {
            (CassetteOutcome::Ok(want), Ok(actual)) => {
                if *want != actual {
                    return Err(ConformanceError::OutcomeMismatch {
                        index,
                        detail: "response body differs from cassette".into(),
                    });
                }
            }
            // LlmError has no PartialEq; an error-class match is the
            // contract (the recorded error reproduced as an error).
            (CassetteOutcome::Err(_), Err(_)) => {}
            (CassetteOutcome::Ok(_), Err(e)) => {
                return Err(ConformanceError::OutcomeMismatch {
                    index,
                    detail: format!("expected success, got error: {e}"),
                });
            }
            (CassetteOutcome::Err(_), Ok(_)) => {
                return Err(ConformanceError::OutcomeMismatch {
                    index,
                    detail: "expected recorded error, got success".into(),
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Usage;

    fn ok_resp(text: &str) -> CompletionResponse {
        CompletionResponse {
            content: Some(text.into()),
            tool_calls: vec![],
            finish_reason: "stop".into(),
            usage: Usage::default(),
        }
    }

    fn cassette() -> Cassette {
        Cassette {
            model: "test-model".into(),
            capabilities: ProviderCapabilities {
                tool_use: true,
                ..Default::default()
            },
            entries: vec![
                CassetteEntry {
                    request: CompletionRequest::default(),
                    outcome: CassetteOutcome::Ok(ok_resp("first")),
                },
                CassetteEntry {
                    request: CompletionRequest::default(),
                    outcome: CassetteOutcome::Err(LlmError::Auth("openai".into())),
                },
            ],
        }
    }

    #[tokio::test]
    async fn replays_responses_in_order() {
        let p = CassetteProvider::new(cassette());
        let r = p.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(r.content.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn replays_recorded_error_variant() {
        let p = CassetteProvider::new(cassette());
        let _ = p.complete(CompletionRequest::default()).await.unwrap();
        let err = p.complete(CompletionRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Auth(p) if p == "openai"));
    }

    #[tokio::test]
    async fn exhausted_cassette_errors_not_panics() {
        let p = CassetteProvider::new(cassette());
        let _ = p.complete(CompletionRequest::default()).await;
        let _ = p.complete(CompletionRequest::default()).await;
        let err = p.complete(CompletionRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Provider { .. }));
    }

    #[test]
    fn reports_cassette_model_and_capabilities() {
        let p = CassetteProvider::new(cassette());
        assert_eq!(p.model_name(), "test-model");
        assert!(p.capabilities().tool_use);
    }

    #[test]
    fn cassette_json_roundtrips() {
        let c = cassette();
        let once = c.to_json();
        let twice = Cassette::from_json(&once).unwrap().to_json();
        assert_eq!(once, twice);
    }

    #[tokio::test]
    async fn conformance_passes_for_matching_replay() {
        let expected = cassette();
        let provider = CassetteProvider::new(cassette());
        run_conformance(&provider, &expected).await.unwrap();
    }

    #[tokio::test]
    async fn conformance_fails_on_model_mismatch() {
        let mut expected = cassette();
        expected.model = "different".into();
        let provider = CassetteProvider::new(cassette());
        let err = run_conformance(&provider, &expected).await.unwrap_err();
        assert!(matches!(err, ConformanceError::ModelMismatch { .. }));
    }

    #[tokio::test]
    async fn recording_then_replay_round_trips() {
        use crate::testing::MockLlmProvider;
        use std::sync::Arc;

        let mock = Arc::new(MockLlmProvider::new("rec-model"));
        mock.set_capabilities(ProviderCapabilities {
            tool_use: true,
            ..Default::default()
        });
        mock.push_text("hello");
        mock.push_error(LlmError::Auth("p".into()));

        let rec = RecordingProvider::new(mock);
        let first = rec.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(first.content.as_deref(), Some("hello"));
        let second = rec
            .complete(CompletionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(second, LlmError::Auth(_)));

        let cas = rec.into_cassette();
        assert_eq!(cas.model, "rec-model");
        assert!(cas.capabilities.tool_use);
        assert_eq!(cas.entries.len(), 2);

        // The recorded cassette replays the same outcomes.
        let replay = CassetteProvider::new(cas);
        assert_eq!(
            replay
                .complete(CompletionRequest::default())
                .await
                .unwrap()
                .content
                .as_deref(),
            Some("hello")
        );
        assert!(matches!(
            replay
                .complete(CompletionRequest::default())
                .await
                .unwrap_err(),
            LlmError::Auth(_)
        ));
    }

    #[test]
    fn cassette_save_load_round_trips() {
        let c = cassette();
        let path = std::env::temp_dir().join(format!(
            "pgsyn-cassette-{}-{}.json",
            std::process::id(),
            "rt"
        ));
        c.save(&path).unwrap();
        let back = Cassette::load(&path).unwrap();
        assert_eq!(c.to_json(), back.to_json());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_rejects_malformed_file() {
        let path = std::env::temp_dir().join(format!(
            "pgsyn-cassette-{}-{}.json",
            std::process::id(),
            "bad"
        ));
        std::fs::write(&path, "{ not json").unwrap();
        let err = Cassette::load(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }

    // PS-5 slice 6: kernel-level coverage for the canonical helper so
    // changes to default_conformance_cassette surface here before the
    // three plugin crates have to compile.

    #[test]
    fn default_cassette_pins_three_outcome_shapes() {
        let c = default_conformance_cassette(
            "m",
            ProviderCapabilities {
                tool_use: true,
                ..Default::default()
            },
        );
        assert_eq!(c.model, "m");
        assert_eq!(c.entries.len(), 3);
        match &c.entries[0].outcome {
            CassetteOutcome::Ok(r) => {
                assert_eq!(r.content.as_deref(), Some("pong"));
                assert!(r.tool_calls.is_empty());
            }
            other => panic!("entry 0 must be Ok(text), got {other:?}"),
        }
        match &c.entries[1].outcome {
            CassetteOutcome::Ok(r) => {
                assert!(r.content.is_none());
                assert_eq!(r.tool_calls.len(), 1);
                assert_eq!(r.tool_calls[0].name, "echo");
            }
            other => panic!("entry 1 must be Ok(tool_call), got {other:?}"),
        }
        match &c.entries[2].outcome {
            CassetteOutcome::Err(LlmError::Auth(p)) => assert_eq!(p, "conformance-provider"),
            other => panic!("entry 2 must be Err(Auth), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_cassette_replays_against_itself() {
        let c = default_conformance_cassette("m", ProviderCapabilities::default());
        let json = c.to_json();
        let replay = CassetteProvider::from_json(&json).unwrap();
        let expected = Cassette::from_json(&json).unwrap();
        run_conformance(&replay, &expected)
            .await
            .expect("canonical cassette must replay cleanly through run_conformance");
    }
}
