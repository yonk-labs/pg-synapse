//! Cassette record/replay for deterministic provider conformance tests
//! (PS-5, slice 1: the replay side + the conformance harness).
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
//! Slice 2 adds the recording side (`RecordingProvider`) and file IO.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::llm::{LlmProvider, ProviderCapabilities};
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse};

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
}
